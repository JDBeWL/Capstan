use std::path::Path;
use std::time::Duration;

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::Accessor;
use lofty::probe::Probe;

use crate::error::{CoreError, Result};
use crate::filename::{self, FolderContext};
use crate::track::{AudioInfo, FieldSource, FieldSources, FileStamp, Track, TrackId};

/// 内嵌封面的原始字节。解码成位图是渲染层的事，内核只负责把字节拿出来。
#[derive(Clone, Debug)]
pub struct Cover {
    pub data: Vec<u8>,
    pub mime: Option<String>,
}

/// 读一首曲目。没有同目录上下文时走这条。目录提示仍然生效（它只依赖路径），
/// 但无法取得「同目录内哪一侧重复」这一信号。
pub fn read_track(path: &Path) -> Result<Track> {
    read_track_in(path, &FolderContext::default())
}

/// 读一首曲目，带同目录上下文。字段按「标签 → 文件名 → 目录名」三级兜底：
/// 标签优先，标签缺失的字段才从文件名推断。三级都取不到时留空，不使用占位值。
pub fn read_track_in(path: &Path, folder: &FolderContext<'_>) -> Result<Track> {
    let tagged = Probe::open(path)
        .map_err(|source| CoreError::Tag {
            path: path.to_path_buf(),
            source,
        })?
        .read()
        .map_err(|source| CoreError::Tag {
            path: path.to_path_buf(),
            source,
        })?;

    // 采样规格来自容器与流头而不是标签。`> 0` 的过滤是把「库返回 0」当成「没读到」。
    let props = tagged.properties();
    let duration: Duration = props.duration();
    let audio = AudioInfo {
        sample_rate: props.sample_rate().filter(|rate| *rate > 0),
        bit_depth: props.bit_depth().filter(|bits| *bits > 0),
        bitrate: props.overall_bitrate().filter(|rate| *rate > 0),
    };
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    // 路径与文件名能给的先全填上，再用标签覆盖。标签缺哪一项，兜底值就留在哪一项上。
    let mut track = infer(path, folder);

    if let Some(title) = clean(tag.and_then(Accessor::title)) {
        track.title = title;
        track.sources.title = FieldSource::Tag;
    }
    if let Some(artist) = clean(tag.and_then(Accessor::artist)) {
        track.artist = Some(artist);
        track.sources.artist = FieldSource::Tag;
    }
    if let Some(album) = clean(tag.and_then(Accessor::album)) {
        track.album = Some(album);
        track.sources.album = FieldSource::Tag;
    }

    track.duration = Some(duration).filter(|d| !d.is_zero());
    track.has_cover = tag.is_some_and(|tag| !tag.pictures().is_empty());
    track.audio = audio;

    Ok(track)
}

/// 完全不读文件内容，只按路径与文件名造一条曲目。
/// 两条路会用到它。标签读失败（坏 ID3、整块标签缺失），以及 [`read_track_in`] 的兜底底稿。
pub fn infer(path: &Path, folder: &FolderContext<'_>) -> Track {
    let dirs = filename::hints_for(path);
    let stem = stem_of(path);
    let guess = filename::guess(&stem, dirs, folder);

    let (title, title_source) = match &guess {
        Some(guess) => (guess.title.clone(), FieldSource::Filename),
        None => (stem, FieldSource::Filename),
    };

    let (artist, artist_source) = match guess.as_ref().and_then(|guess| guess.artist.clone()) {
        Some(artist) => (Some(artist), FieldSource::Filename),
        None => (dirs.artist.map(ToOwned::to_owned), FieldSource::Folder),
    };

    let (mut album, album_source) = match guess.as_ref().and_then(|guess| guess.album.clone()) {
        Some(album) => (Some(album), FieldSource::Filename),
        None => (dirs.album.map(ToOwned::to_owned), FieldSource::Folder),
    };

    // 父目录同时被当成了艺术家和专辑，说明它就是那位歌手的文件夹而不是一张专辑
    // （`周杰伦/周杰伦 - 七里香.flac`）。造一张叫「周杰伦」的专辑没有意义。
    if album == artist {
        album = None;
    }

    Track {
        id: TrackId::from_path(path),
        path: path.to_path_buf(),
        title,
        artist,
        album,
        duration: None,
        has_cover: false,
        audio: AudioInfo::default(),
        sources: FieldSources {
            title: title_source,
            artist: artist_source,
            album: album_source,
        },
        // 读这一首时的指纹。唯一一次 stat 在这里做，扫描器只负责拿它和现值比。
        stamp: FileStamp::of(path),
    }
}

/// 按需读取封面字节。只在真正需要显示时调用（当前曲目 + 列表可视区）。
pub fn read_cover(path: &Path) -> Result<Option<Cover>> {
    let tagged = Probe::open(path)
        .map_err(|source| CoreError::Tag {
            path: path.to_path_buf(),
            source,
        })?
        .read()
        .map_err(|source| CoreError::Tag {
            path: path.to_path_buf(),
            source,
        })?;

    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(None);
    };

    let picture = tag
        .get_picture_type(lofty::picture::PictureType::CoverFront)
        .or_else(|| tag.pictures().first());

    let Some(picture) = picture else {
        return Ok(None);
    };

    Ok(Some(Cover {
        data: picture.data().to_vec(),
        mime: picture.mime_type().map(ToString::to_string),
    }))
}

/// 标签值归一。去首尾空白，空的当没有。一个全空格的标题不该压住文件名带来的推断。
fn clean(value: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn stem_of(path: &Path) -> String {
    path.file_stem().map_or_else(
        || path.to_string_lossy().into_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}
