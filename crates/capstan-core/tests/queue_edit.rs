//! 队列编辑。队列本来是「跟着你看的列表」的派生状态，手动插删一次就冻结成显式列表；
//! 这些用例钉的是冻结之后的那几条不变量。

use std::path::PathBuf;

use capstan_core::{PlayMode, Playlist, Track, TrackId};

fn track(n: u64) -> Track {
    let path = PathBuf::from(format!("/music/{n}.flac"));
    let mut track = Track::fallback(&path);
    track.title = format!("Track {n}");
    track
}

fn id(n: u64) -> TrackId {
    track(n).id
}

fn playlist(count: u64) -> Playlist {
    let mut playlist = Playlist::new();
    playlist.push((0..count).map(track));
    playlist
}

fn order_ids(playlist: &Playlist) -> Vec<TrackId> {
    playlist.order().to_vec()
}

#[test]
fn 插入不在队列里的曲目会冻结成显式队列() {
    let mut playlist = playlist(4);
    playlist.set_queue(Some(vec![id(0), id(1)]), 0);

    assert_eq!(playlist.queue_insert(&[id(3)], 1), 1);

    assert_eq!(order_ids(&playlist), [id(0), id(3), id(1)]);
    assert_eq!(
        playlist.queue().map(<[TrackId]>::to_vec),
        Some(order_ids(&playlist)),
        "编辑即冻结：队列从这一刻起是显式的"
    );
}

#[test]
fn 整库队列里挪位同样会冻结() {
    let mut playlist = playlist(4);
    assert!(playlist.queue().is_none(), "整库播放时队列跟着曲库走");

    assert_eq!(
        playlist.queue_insert(&[id(3)], 1),
        0,
        "这一首本来就在队列里，算挪位不算新增"
    );

    assert_eq!(order_ids(&playlist), [id(0), id(3), id(1), id(2)]);
    assert!(playlist.queue().is_some(), "挪位同样是编辑，一样要冻结");
}

#[test]
fn 下一首插在光标之后() {
    let mut playlist = playlist(3);
    playlist.jump_to(id(0));

    let at = playlist.cursor() + 1;
    playlist.queue_insert(&[id(2)], at);

    assert_eq!(order_ids(&playlist), [id(0), id(2), id(1)]);
    assert_eq!(playlist.current_id(), Some(id(0)), "光标不该被插歌挪走");
}

#[test]
fn 已在队列里的曲目挪位而不是重复() {
    let mut playlist = playlist(4);

    assert_eq!(playlist.queue_insert(&[id(3)], 0), 0, "全是挪位，没有新增");

    assert_eq!(order_ids(&playlist), [id(3), id(0), id(1), id(2)]);
    assert_eq!(
        order_ids(&playlist).len(),
        4,
        "重复 id 会让 jump_to 只命中第一个"
    );
}

#[test]
fn 随机模式插歌不打乱剩下的顺序() {
    let mut playlist = playlist(8);
    playlist.set_mode(PlayMode::Shuffle, 0x5eed);
    let before = order_ids(&playlist);
    playlist.jump_to(before[0]);

    let at = playlist.cursor() + 1;
    playlist.queue_insert(&[before[5]], at);

    // 被挪走的那首在两边的比法里都要剔掉，剩下的相对顺序必须一模一样。
    let moved = before[5];
    let keep = |ids: Vec<TrackId>| -> Vec<TrackId> {
        ids.into_iter().filter(|track| *track != moved).collect()
    };
    assert_eq!(
        keep(order_ids(&playlist)),
        keep(before),
        "插一首歌不该把剩下的随机序重洗"
    );
}

#[test]
fn 未知_id_被丢弃且不冻结队列() {
    let mut playlist = playlist(3);

    assert_eq!(playlist.queue_insert(&[TrackId(999)], 1), 0);

    assert!(playlist.queue().is_none(), "什么都没变就不该冻结");
    assert_eq!(order_ids(&playlist), [id(0), id(1), id(2)]);
}

#[test]
fn 从队列移除不动曲库() {
    let mut playlist = playlist(3);
    playlist.jump_to(id(0));

    assert!(playlist.queue_remove(id(2)));

    assert_eq!(playlist.tracks().len(), 3, "队列和曲库是两回事");
    assert_eq!(order_ids(&playlist), [id(0), id(1)]);
}

#[test]
fn 移除正在播的那首被拒绝() {
    let mut playlist = playlist(3);
    playlist.jump_to(id(1));

    assert!(!playlist.queue_remove(id(1)));

    assert_eq!(order_ids(&playlist), [id(0), id(1), id(2)]);
}

#[test]
fn 移除前面的曲目之后光标仍指向同一首() {
    let mut playlist = playlist(4);
    playlist.jump_to(id(2));

    assert!(playlist.queue_remove(id(0)));

    assert_eq!(playlist.current_id(), Some(id(2)));
}

#[test]
fn 一路删到只剩正在播的那首() {
    let mut playlist = playlist(3);
    playlist.jump_to(id(1));

    assert!(playlist.queue_remove(id(0)));
    assert!(playlist.queue_remove(id(2)));

    assert_eq!(order_ids(&playlist), [id(1)]);
    assert!(!playlist.queue_remove(id(1)), "最后一首就是正在播的那首");
}
