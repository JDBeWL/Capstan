//! 基于 rodio 的播放后端。
//! rodio 只负责把采样送到声卡；解码、重采样、声道转换、增益包络、软削波全在 `crate::decode`。
//! 本类型 `!Send`（`MixerDeviceSink` 底层是 cpal 的 `Stream`），只在音频线程内创建和使用。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rodio::source::UniformSourceIterator;
use rodio::{ChannelCount, DeviceSinkBuilder, MixerDeviceSink, Player, SampleRate};

use crate::backend::{AudioBackend, BackendNotice};
use crate::decode::{
    ChannelConvertSource, GainControl, GainRamp, GaplessSource, GaplessState, LfeConfig,
    LockFreeSource, NextSlot, Segment, SoftClip, TrackDecoder, WhenEmpty, new_slot,
};
use crate::device;
use crate::error::{AudioError, Result};

/// 淡入、淡出与换设备交叉淡化的时长。60ms 够抹平接缝。
const FADE: Duration = Duration::from_millis(60);
/// 淡出结束后再多保留一段时间才丢弃旧输出流，确保增益确实到达 0。
const FADE_MARGIN: Duration = Duration::from_millis(20);

/// 一次链路构建的产物，含音源、时长、用于淡出的增益手柄，以及无缝换段所需的槽位与边界计数。
struct BuiltSource {
    source: Box<dyn rodio::Source<Item = f32> + Send>,
    duration: Duration,
    gain: Arc<GainControl>,
    state: Arc<GaplessState>,
    next: NextSlot,
}

/// 当前链路的段边界状态与下一段槽位。没有链路时为空。
#[derive(Clone)]
struct Chain {
    state: Arc<GaplessState>,
    next: NextSlot,
}

pub struct RodioBackend {
    /// 必须一直持有。一旦 drop，音频流关闭，声音立刻消失。
    device: MixerDeviceSink,
    player: Player,
    /// 当前输出设备的稳定标识；`None` 表示跟随系统默认。
    device_id: Option<String>,
    current: Option<Loaded>,
    /// 当前链路，用于判断段边界与替换下一段。
    chain: Option<Chain>,
    /// 已经排进链路、尚未开始的下一段元信息。
    next: Option<Loaded>,
    /// 交接期间收到的下一段路径，交接落地后装入新链路的槽位。
    /// 手动切歌也走交接，缺此记录时切歌后的曲目不会获得预载。
    wanted_next: Option<PathBuf>,
    /// 已上报的段序号，防止同一边界重复上报。
    promoted_seq: u64,
    /// 当前链路的增益手柄，淡出时用它把旧链路推到 0。
    gain: Option<Arc<GainControl>>,
    /// 淡出期间挂起的交接动作。
    pending: Option<Pending>,
    /// 平滑切换时正在淡出的上一条输出流，到点才丢。
    previous: Option<Previous>,
    volume: f32,
    /// 播放器是否已「停」（`stop()` 之后、尚未重新装载）。`stop()` 只立标志，真正丢掉音源
    /// 要等淡出结束，由 `tick()` 与各公共入口推进。
    stopped: bool,
    /// 换设备时是否做交叉淡化。
    smooth_switch: bool,
    /// 待上报给音频线程的异步通知，目前只有无缝换段。
    notices: VecDeque<BackendNotice>,
}

struct Loaded {
    path: PathBuf,
    duration: Duration,
    /// 当前音源的起点。rodio 的 `get_pos()` 在每次 `append` 后从 0 重新计数，
    /// 重建式 seek 之后要把它加回去，否则进度条每次都跳回开头。
    base: Duration,
    /// 重建时需要保持原来的播放或暂停状态。
    playing: bool,
}

/// 已经建好、等旧链路淡出之后再交付的动作。
enum Handoff {
    /// 清空队列（`stop()`）
    Stop,
    Swap {
        built: BuiltSource,
        loaded: Loaded,
        autoplay: bool,
    },
}

struct Pending {
    action: Handoff,
    deadline: Instant,
}

impl Pending {
    /// 挂起期间对外报的位置，交接落地之后才真的在那里。
    fn position(&self) -> Duration {
        match &self.action {
            Handoff::Stop => Duration::ZERO,
            Handoff::Swap { loaded, .. } => loaded.base,
        }
    }
}

/// 淡出中的上一条输出流。两个句柄都要持有到点。丢掉 `Player` 会立刻停掉它的音源，
/// 丢掉 `MixerDeviceSink` 会关掉整条流。
struct Previous {
    _player: Player,
    _device: MixerDeviceSink,
    deadline: Instant,
}

/// 打开指定设备的输出流，返回流与它实际对应的设备标识。
///
/// 标识过期、设备已拔出时回退到系统默认设备（此时标识视为「跟随默认」）。
fn open_sink(device_id: Option<&str>) -> Result<(MixerDeviceSink, Option<String>)> {
    let selected = device_id.and_then(|id| device::resolve_device(id).map(|device| (id, device)));
    if device_id.is_some() && selected.is_none() {
        log::warn!("选定的输出设备已不可用，回退到系统默认设备");
    }

    let (builder, resolved_id) = match selected {
        Some((id, cpal_device)) => (DeviceSinkBuilder::from_device(cpal_device), Some(id)),
        None => (DeviceSinkBuilder::from_default_device(), None),
    };

    let builder = builder.map_err(|err| AudioError::DeviceOpen(err.to_string()))?;
    // `open_sink_or_fallback` 失败时会在同一台设备上试其它可用配置，不换设备。
    let mut sink = builder
        .open_sink_or_fallback()
        .map_err(|err| AudioError::DeviceOpen(err.to_string()))?;
    // 流的生命周期由我们按淡出时机管理，不需要 rodio 每次 drop 都往 stderr 报一句。
    sink.log_on_drop(false);

    Ok((sink, resolved_id.map(str::to_owned)))
}

/// 组装一条完整的链路。当前段与预载的下一段先串成连续音源，再统一施加增益包络
/// 与软削波，故自然换段时增益不会重新从 0 爬升。目标采样率与声道数由输出流决定，
/// 换设备时按新流重建。
fn build_source(sink: &MixerDeviceSink, path: &Path, start: Duration) -> Result<BuiltSource> {
    build_source_with_slot(sink, path, start, new_slot())
}

/// 与 [`build_source`] 相同，但下一段槽位由调用方提供。传入已有槽位可保留其中的预载，
/// 传 [`new_slot`] 则得到一条空槽位的链路。
fn build_source_with_slot(
    sink: &MixerDeviceSink,
    path: &Path,
    start: Duration,
    next: NextSlot,
) -> Result<BuiltSource> {
    let config = sink.config();
    build_source_for_slot(
        config.sample_rate(),
        config.channel_count(),
        path,
        start,
        next,
    )
}

#[cfg(test)]
fn build_source_for(
    sample_rate: SampleRate,
    target_channels: ChannelCount,
    path: &Path,
    start: Duration,
) -> Result<BuiltSource> {
    build_source_for_slot(sample_rate, target_channels, path, start, new_slot())
}

fn build_source_for_slot(
    sample_rate: SampleRate,
    target_channels: ChannelCount,
    path: &Path,
    start: Duration,
    next: NextSlot,
) -> Result<BuiltSource> {
    let (segment, duration) = build_segment_for(sample_rate, target_channels, path, start)?;
    let state = Arc::new(GaplessState::default());
    let chained = GaplessSource::new(
        segment,
        Arc::clone(&next),
        Arc::clone(&state),
        sample_rate,
        target_channels,
    );

    // 增益按设备采样率折算，必须放在重采样之后。
    let control = Arc::new(GainControl::new());
    let ramped = GainRamp::new(
        chained,
        FADE,
        sample_rate.get(),
        target_channels.get(),
        Arc::clone(&control),
    );
    let clipped = SoftClip::new(ramped);

    Ok(BuiltSource {
        source: Box::new(clipped),
        duration,
        gain: control,
        state,
        next,
    })
}

/// 只建一段，不含增益与削波。接段时不能重新淡入，增益包络必须包住整条链。
fn build_segment_for(
    sample_rate: SampleRate,
    target_channels: ChannelCount,
    path: &Path,
    start: Duration,
) -> Result<(Box<dyn rodio::Source<Item = f32> + Send>, Duration)> {
    let mut decoder = TrackDecoder::open(path)?;
    let duration = decoder.total_duration().unwrap_or(Duration::ZERO);

    if !start.is_zero() {
        decoder.seek(start)?;
    }

    let source_channels =
        std::num::NonZeroU16::new(decoder.channels()).unwrap_or(ChannelCount::MIN);

    let source = LockFreeSource::new(decoder, WhenEmpty::PadWithSilence)?;

    // 这一步只对齐采样率、保持声道。LFE 低通要按声源的采样率算，必须排在下混之前。
    // `LockFreeSource` 恒返回 `current_span_len() == None`，rodio 的内部重采样
    // 不会按跨度分块，所以必须在交给 mixer 之前显式对齐。
    let resampled = UniformSourceIterator::new(source, source_channels, sample_rate);
    let converted = ChannelConvertSource::new(resampled, target_channels, LfeConfig::default());

    Ok((Box::new(converted), duration))
}

impl RodioBackend {
    pub fn open() -> Result<Self> {
        Self::open_with_device(None)
    }

    /// `device_id` 为 `None` 时跟随系统默认设备。
    pub fn open_with_device(device_id: Option<&str>) -> Result<Self> {
        let (sink, device_id) = open_sink(device_id)?;

        let player = Player::connect_new(sink.mixer());
        // 新建的 Player 默认就是播放中，先压回暂停，避免 load 之后立刻出声。
        player.pause();

        Ok(Self {
            device: sink,
            player,
            device_id,
            current: None,
            chain: None,
            next: None,
            wanted_next: None,
            promoted_seq: 0,
            gain: None,
            pending: None,
            previous: None,
            volume: 1.0,
            stopped: false,
            smooth_switch: false,
            notices: VecDeque::new(),
        })
    }

    /// 清空队列并把新音源接上，同时恢复之前的音量。
    fn swap_in(&self, source: Box<dyn rodio::Source<Item = f32> + Send>, autoplay: bool) {
        self.player.clear();
        self.player.set_volume(self.volume);
        self.player.append(source);
        if autoplay {
            self.player.play();
        } else {
            self.player.pause();
        }
    }

    /// 收尾。落地到点的挂起交接，丢弃到点的旧输出流。
    fn sweep(&mut self) {
        let now = Instant::now();

        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.deadline <= now)
            && let Some(pending) = self.pending.take()
        {
            self.apply_handoff(pending.action);
        }

        if self
            .previous
            .as_ref()
            .is_some_and(|previous| previous.deadline <= now)
        {
            self.previous = None;
        }
    }

    fn apply_handoff(&mut self, action: Handoff) {
        self.pending = None;
        match action {
            Handoff::Stop => {
                self.player.stop();
                self.gain = None;
                self.chain = None;
                self.next = None;
            }
            Handoff::Swap {
                built,
                loaded,
                autoplay,
            } => {
                self.swap_in(built.source, autoplay);
                self.gain = Some(built.gain);
                self.current = Some(loaded);
                self.chain = Some(Chain {
                    state: built.state,
                    next: built.next,
                });
                self.next = None;
                self.promoted_seq = 0;
                self.stopped = false;

                // 交接期间排队的曲目此时才有链路可挂载。
                if let Some(path) = self.wanted_next.take()
                    && let Err(err) = self.install_next(&path)
                {
                    log::warn!("预载下一首失败：{err}");
                }
            }
        }
    }

    /// 交付下一段音频。链路正在出声就先淡出再换；已经静着就直接换。
    fn hand_off(&mut self, action: Handoff) {
        // 已经在淡出。换成新动作仍等到点交接，旧链路增益已趋近 0。
        if let Some(pending) = self.pending.as_mut() {
            pending.action = action;
            return;
        }

        // 队列已经放空时（一首自然播完、随后自动切下一曲）没有声音可淡，再等一拍只多一次起播延迟。
        let audible = !self.player.empty()
            && self.gain.is_some()
            && self.current.as_ref().is_some_and(|current| current.playing);
        if !audible {
            self.apply_handoff(action);
            return;
        }

        if let Some(gain) = &self.gain {
            gain.set_target(0.0);
        }
        self.pending = Some(Pending {
            action,
            deadline: Instant::now() + FADE + FADE_MARGIN,
        });
    }

    /// 挂起的交接要跟随最新的播放意图。淡出那几十毫秒里用户可能又按了一次。
    fn aim_pending_at(&mut self, autoplay: bool) {
        if let Some(pending) = self.pending.as_mut()
            && let Handoff::Swap {
                autoplay: wanted, ..
            } = &mut pending.action
        {
            *wanted = autoplay;
        }
    }

    /// 清除下一段，含槽位、元信息与交接待办。
    /// 换曲与停止都必须先调用，否则淡出的几十毫秒内旧链跨段会播出过期曲目。
    fn clear_next(&mut self) {
        if let Some(chain) = self.chain.as_ref()
            && let Ok(mut slot) = chain.next.lock()
        {
            *slot = None;
        }
        self.next = None;
        self.wanted_next = None;
    }

    /// 将一段预载音源装入当前链路的槽位。必须先清旧槽位再构建，
    /// 否则构建期间跨过边界会播出过期曲目，此时宁可该曲目没有预载。
    fn install_next(&mut self, path: &Path) -> Result<()> {
        let Some(chain) = self.chain.as_ref() else {
            return Ok(());
        };
        let slot = Arc::clone(&chain.next);
        if let Ok(mut guard) = slot.lock() {
            *guard = None;
        }
        self.next = None;

        let config = self.device.config();
        let (segment, duration) = build_segment_for(
            config.sample_rate(),
            config.channel_count(),
            path,
            Duration::ZERO,
        )?;
        // 元信息必须先于槽位就位，否则音频回调可能在下一个样本就跨段取走音源。
        self.next = Some(Loaded {
            path: path.to_path_buf(),
            duration,
            base: Duration::ZERO,
            playing: true,
        });
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(Segment::new(segment));
        } else {
            self.next = None;
        }
        Ok(())
    }

    /// 段边界由音源上报。此处只将其转换为元信息变更，音频本身已经连续。
    fn advance_if_needed(&mut self) {
        // 有挂起的交接时整条链将被替换，旧链跨段不产生有效的元信息。
        // 否则淡出期间恰好跨段，会先上报换段，随后又切到用户选择的曲目。
        if self.pending.is_some() {
            return;
        }

        let Some(chain) = self.chain.as_ref() else {
            return;
        };
        let seq = chain.state.segment_seq();
        if seq <= self.promoted_seq {
            return;
        }
        self.promoted_seq = seq;
        let Some(next) = self.next.take() else {
            return;
        };
        self.current = Some(Loaded {
            path: next.path.clone(),
            duration: next.duration,
            base: Duration::ZERO,
            playing: true,
        });
        self.notices.push_back(BackendNotice::Advanced {
            path: next.path,
            duration: next.duration,
        });
    }

    /// 按当前曲目重建一条音源链路并接上；没装载过时是无操作。
    fn rebuild_current(&mut self) -> Result<()> {
        // 旧音源整条要被换掉，挂起的交接与下一段一起作废。
        self.pending = None;
        self.clear_next();

        let Some(current) = self.current.as_ref() else {
            return Ok(());
        };
        let (path, start) = (current.path.clone(), current.base);

        let built = build_source(&self.device, &path, start)?;
        self.swap_in(built.source, false);
        self.gain = Some(built.gain);
        self.chain = Some(Chain {
            state: built.state,
            next: built.next,
        });
        self.next = None;
        self.promoted_seq = 0;
        Ok(())
    }

    /// 换到另一条输出流上。先把新流建好、曲目接上，成功之后才替换旧流。
    /// 中途失败时旧流原封不动，声音不会断。
    fn apply_device(&mut self, device_id: Option<&str>) -> Result<()> {
        self.clear_next();
        let (sink, resolved_id) = open_sink(device_id)?;

        let player = Player::connect_new(sink.mixer());
        player.set_volume(self.volume);
        player.pause();

        // 从「此刻播到的位置」续上，不能用 `current.base`。base 只在 load / seek 时更新，
        // 用它重建会丢弃上次手动跳转后的已播放片段，进度直接倒退。
        let resume_at = self.current.is_some().then(|| self.position());

        let mut gain = None;
        let mut chain = None;
        if let (Some(current), Some(start)) = (self.current.as_ref(), resume_at) {
            let built = build_source(&sink, &current.path, start)?;
            gain = Some(built.gain);
            player.append(built.source);
            chain = Some(Chain {
                state: built.state,
                next: built.next,
            });
            if current.playing {
                player.play();
            }
        }

        let old_player = std::mem::replace(&mut self.player, player);
        let old_device = std::mem::replace(&mut self.device, sink);
        self.pending = None;
        self.chain = chain;
        self.next = None;
        self.promoted_seq = 0;

        // 平滑切换。旧流继续出声、增益推向 0，几十毫秒后才断开，覆盖新流预建的空窗。
        // 不平滑时立即断开。
        if self.smooth_switch && self.gain.is_some() {
            if let Some(control) = self.gain.take() {
                control.set_target(0.0);
            }
            self.previous = Some(Previous {
                _player: old_player,
                _device: old_device,
                deadline: Instant::now() + FADE + FADE_MARGIN,
            });
        }

        self.gain = gain;
        self.device_id = resolved_id;
        self.stopped = false;

        // 新流以 start 为基准，base 必须跟着走，否则 `position()` 会重复计入。
        if let (Some(current), Some(start)) = (self.current.as_mut(), resume_at) {
            current.base = start;
        }
        Ok(())
    }
}

impl AudioBackend for RodioBackend {
    fn load(&mut self, path: &Path, autoplay: bool) -> Result<Duration> {
        self.sweep();
        self.clear_next();

        let built = build_source(&self.device, path, Duration::ZERO)?;
        let duration = built.duration;
        let loaded = Loaded {
            path: path.to_path_buf(),
            duration,
            base: Duration::ZERO,
            playing: autoplay,
        };
        self.hand_off(Handoff::Swap {
            built,
            loaded,
            autoplay,
        });

        Ok(duration)
    }

    /// 预载下一段。`after` 是调用方记录的当前曲目，不匹配即视为过期命令并丢弃。
    /// 失败以 `Err` 返回，调用方只记日志，因为预载并非正确性前提，自然结束时仍会装载并报错。
    fn set_next(&mut self, after: &Path, next: Option<&Path>) -> Result<()> {
        self.sweep();
        // 队列可能在段边界之后、本次 tick 之前变化。先提交已跨过的段，
        // 否则其元信息会被当作「下一首」覆盖，界面与音频错位。
        self.advance_if_needed();

        // 淡出交接期间新链路尚未落地，先记录，等交接完成再装入。
        if let Some(pending) = &self.pending {
            if let Handoff::Swap { loaded, .. } = &pending.action
                && loaded.path == after
            {
                self.wanted_next = next.map(Path::to_path_buf);
            }
            return Ok(());
        }

        let Some(current) = self.current.as_ref() else {
            return Ok(());
        };
        if current.path != after || self.stopped || self.player.empty() {
            return Ok(());
        }

        if let Some(path) = next {
            if self.next.as_ref().is_some_and(|loaded| loaded.path == path) {
                return Ok(());
            }
            self.wanted_next = None;
            self.install_next(path)
        } else {
            self.wanted_next = None;
            if let Some(chain) = self.chain.as_ref()
                && let Ok(mut slot) = chain.next.lock()
            {
                *slot = None;
            }
            self.next = None;
            Ok(())
        }
    }

    /// 队列空了（`stop()` 之后、或一首自然播完）时 `player.play()` 只放下暂停标志，
    /// 没有音源可播必须按当前曲目重建链路，否则是「无声的正在播放」。
    fn play(&mut self) -> Result<()> {
        self.sweep();
        self.aim_pending_at(true);

        if self.stopped || self.player.empty() {
            self.rebuild_current()?;
        }

        self.player.play();
        self.stopped = false;
        if let Some(current) = self.current.as_mut() {
            current.playing = true;
        }
        Ok(())
    }

    fn pause(&mut self) {
        self.sweep();
        self.aim_pending_at(false);
        self.player.pause();
        if let Some(current) = self.current.as_mut() {
            current.playing = false;
        }
    }

    fn stop(&mut self) {
        self.sweep();
        self.clear_next();
        // 先决定淡不淡，再改状态，判据要看此刻是不是真的在出声。
        self.hand_off(Handoff::Stop);
        // 保留 current。stop 之后按「上一曲」重播当前曲还要用；起点归零。
        if let Some(current) = self.current.as_mut() {
            current.base = Duration::ZERO;
            current.playing = false;
        }
        self.stopped = true;
    }

    /// 重建式 seek，不用 `player.try_seek()`。有界通道里已缓存几秒采样，原地跳转既复杂又不精确。
    fn seek(&mut self, position: Duration) -> Result<()> {
        self.sweep();
        self.clear_next();

        let Some(current) = self.current.as_ref() else {
            return Ok(()); // 还没装载任何东西，seek 无意义
        };

        let path = current.path.clone();
        let duration = current.duration;
        let was_playing = current.playing;
        let target = if duration > Duration::ZERO {
            position.min(duration)
        } else {
            position
        };

        let built = build_source(&self.device, &path, target)?;
        let loaded = Loaded {
            path,
            duration,
            base: target,
            playing: was_playing,
        };
        self.hand_off(Handoff::Swap {
            built,
            loaded,
            autoplay: was_playing,
        });
        self.stopped = false;

        Ok(())
    }

    fn set_volume(&mut self, volume: f32) {
        self.sweep();
        self.volume = volume.clamp(0.0, 1.0);
        self.player.set_volume(self.volume);
    }

    fn position(&self) -> Duration {
        // 挂起期间报的是交接目标。此刻出声的仍是上一段，报它会导致进度条反复跳动。
        if let Some(pending) = &self.pending {
            return pending.position();
        }

        let Some(current) = self.current.as_ref() else {
            return Duration::ZERO;
        };
        let Some(chain) = self.chain.as_ref() else {
            return current.base;
        };
        let config = self.device.config();
        let position = current.base
            + chain
                .state
                .segment_position(config.sample_rate().get(), config.channel_count().get());
        if current.duration > Duration::ZERO {
            position.min(current.duration)
        } else {
            position
        }
    }

    fn is_finished(&self) -> bool {
        self.player.empty()
    }

    fn tick(&mut self) {
        self.sweep();
        self.advance_if_needed();
    }

    fn take_notice(&mut self) -> Option<BackendNotice> {
        self.notices.pop_front()
    }

    fn set_device(&mut self, device: Option<&str>) -> Result<()> {
        self.sweep();
        self.apply_device(device)
    }

    fn current_device(&self) -> Option<String> {
        self.device_id.clone()
    }

    fn set_smooth_switch(&mut self, enabled: bool) {
        self.smooth_switch = enabled;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 用例素材的采样率。挑 8k 是为了链路跑得快，与格式无关。
    const SOURCE_RATE: u32 = 8_000;

    /// 淡入的帧数。窗内的增益本来就在爬升，断言一律跳过它。
    const FADE_FRAMES: usize = SOURCE_RATE as usize * 60 / 1000;

    fn rate(value: u32) -> SampleRate {
        std::num::NonZeroU32::new(value).unwrap_or(SampleRate::MIN)
    }

    fn channels(value: u16) -> ChannelCount {
        std::num::NonZeroU16::new(value).unwrap_or(ChannelCount::MIN)
    }

    fn wav(dir: &tempfile::TempDir, channels: u16, frames: usize) -> PathBuf {
        const BITS: u16 = 16;
        let block_align = channels * BITS / 8;
        let data_len = (frames * channels as usize * 2) as u32;

        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&SOURCE_RATE.to_le_bytes());
        bytes.extend_from_slice(&(SOURCE_RATE * u32::from(block_align)).to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&BITS.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());

        // 每路一个常数，6 声道按 FL FR FC LFE SL SR 的次序取不同值。
        let level = |channel: usize| [0.5_f32, 0.25, 0.2, 0.9, 0.1, 0.2][channel % 6];
        for _ in 0..frames {
            for channel in 0..channels as usize {
                let value = level(channel).clamp(-1.0, 1.0);
                let quantized = (value * 32_767.0).round() as i16;
                bytes.extend_from_slice(&quantized.to_le_bytes());
            }
        }

        let path = dir.path().join(format!("{channels}ch.wav"));
        std::fs::write(&path, bytes).expect("写入测试 WAV 失败");
        path
    }

    /// 先让解码线程把素材写入通道再读。欠载时 `LockFreeSource` 会输出 0.0 维持流，
    /// 样本数与声道相位都会对不上。
    fn drain(source: Box<dyn rodio::Source<Item = f32> + Send>) -> Vec<f32> {
        std::thread::sleep(Duration::from_millis(100));
        source.collect()
    }

    #[test]
    fn a_matching_layout_passes_through_untouched() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = wav(&dir, 2, 2_000);

        let built = build_source_for(rate(SOURCE_RATE), channels(2), &path, Duration::ZERO)
            .expect("构建链路应当成功");
        assert!(
            (built.duration.as_secs_f64() - 0.25).abs() < 0.01,
            "{:?}",
            built.duration
        );

        let samples = drain(built.source);
        assert_eq!(samples.len(), 4_000, "同格式不该增减样本");

        // 淡入之后增益恒为 1.0，样本应当原样出来（16-bit 量化误差在 1/32767 量级）
        for (index, sample) in samples.iter().enumerate().skip(FADE_FRAMES * 2) {
            let wanted = if index % 2 == 0 { 0.5 } else { 0.25 };
            assert!(
                (sample - wanted).abs() < 2e-3,
                "第 {index} 个样本 = {sample}，期望约 {wanted}"
            );
        }
    }

    #[test]
    fn surround_is_downmixed_to_the_device_channel_count() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = wav(&dir, 6, 2_000);

        let built = build_source_for(rate(SOURCE_RATE), channels(2), &path, Duration::ZERO)
            .expect("构建链路应当成功");
        let samples = drain(built.source);
        assert_eq!(samples.len(), 4_000, "6 声道应当被压成 2 声道");

        // 顺序为 FL FR FC LFE SL SR，LFE 默认不参与，归一化系数 0.707。
        let expected_left = (0.5 + 0.2 * 0.707 + 0.1 * 0.707) * 0.707;
        let expected_right = (0.25 + 0.2 * 0.707 + 0.2 * 0.707) * 0.707;
        let mut checked = 0;
        for (index, sample) in samples.iter().enumerate().skip(FADE_FRAMES * 2) {
            let wanted = if index % 2 == 0 {
                expected_left
            } else {
                expected_right
            };
            assert!(
                (sample - wanted).abs() < 3e-3,
                "第 {index} 个样本 = {sample}，期望约 {wanted}"
            );
            checked += 1;
        }
        assert!(checked > 0, "淡入窗比素材还长，这个用例就没验到东西");
    }

    /// 预载的下一段必须与当前段样本级衔接，不插零、不重新淡入。
    #[test]
    fn a_preloaded_next_track_joins_without_a_fade_or_gap() {
        let first_dir = tempfile::tempdir().expect("创建临时目录失败");
        let second_dir = tempfile::tempdir().expect("创建临时目录失败");
        let first = wav(&first_dir, 2, 4_000);
        let second = wav(&second_dir, 2, 4_000);

        let built = build_source_for(rate(SOURCE_RATE), channels(2), &first, Duration::ZERO)
            .expect("构建当前段应当成功");
        let (segment, _) =
            build_segment_for(rate(SOURCE_RATE), channels(2), &second, Duration::ZERO)
                .expect("构建预载段应当成功");
        *built.next.lock().expect("槽位锁") = Some(Segment::new(segment));

        let samples = drain(built.source);
        assert_eq!(samples.len(), 16_000, "两段都应当完整交出");

        // 每段 4000 帧 × 2 声道。段边界上增益必须仍是 1.0，不能重新淡入。
        let boundary = 8_000;
        for (index, sample) in samples.iter().enumerate().skip(boundary - 200).take(400) {
            let wanted = if index % 2 == 0 { 0.5 } else { 0.25 };
            assert!(
                (sample - wanted).abs() < 2e-3,
                "第 {index} 个样本 = {sample}，边界出现静音或淡入凹陷"
            );
        }
        assert_eq!(built.state.segment_seq(), 1, "边界计数应当前进一次");
    }

    #[test]
    fn the_chain_resamples_to_the_device_rate() {
        const FRAMES: usize = 4_000;

        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let path = wav(&dir, 2, FRAMES);

        let built = build_source_for(rate(48_000), channels(2), &path, Duration::ZERO)
            .expect("构建链路应当成功");
        let samples = drain(built.source);

        // 8k → 48k 是 6 倍。
        let expected = FRAMES * 2 * 6;
        let ratio = samples.len() as f64 / expected as f64;
        assert!(
            (ratio - 1.0).abs() < 0.01,
            "解出 {} 个样本，期望约 {expected} 个",
            samples.len()
        );

        // 淡入窗按输出采样率折算。
        let fade_samples = 48_000 / 1000 * 60 * 2;
        for (index, sample) in samples.iter().enumerate().skip(fade_samples) {
            let wanted = if index % 2 == 0 { 0.5 } else { 0.25 };
            assert!(
                (sample - wanted).abs() < 3e-3,
                "第 {index} 个样本 = {sample}，期望约 {wanted}"
            );
        }
    }
}
