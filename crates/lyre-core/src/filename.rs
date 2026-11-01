//! 从文件名推断缺失的标签。
//!
//! 文件名本身不包含「谁是歌手」这一信息。`周杰伦 - 七里香` 与 `七里香 - 周杰伦` 在语法上完全同构，
//! 单条正则无法判定方向。因此此处不以「匹配即结论」，而是先切分、再对「角色分配」打分，
//! 最后按置信度门限决定是否采信。判据全部来自文件名之外的证据：
//!
//! 1. **目录结构**，父目录作为专辑、祖父目录作为艺术家。人工整理过的目录比文件名可靠。
//! 2. **库内已知艺人**，已有标签的曲目可提供一份名字词典。
//! 3. **同目录一致性**。一个专辑目录内艺术家重复十余次，标题则不会。重复出现者即为艺术家。
//!
//! 三条证据均缺失时，仅剩「歌手在前」的位置先验，即 `PRIOR_ARTIST_FIRST`，恰好通过门限，
//! 此即中文曲库的主流写法。证据不足时不作判定，由调用方保留原值。

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// 「歌手在前」的位置先验。它只有这么点分量，随便一条外部证据都能压过去。
const PRIOR_ARTIST_FIRST: i32 = 2;
/// 认定一个候选成立所需的最低分。
const ACCEPT: i32 = 2;
/// 第一名要领先第二名多少才算「没有歧义」。
const MARGIN: i32 = 1;
/// 括号内容的最长保留长度，超过就只当垃圾丢掉。
const MAX_BRACKET: usize = 64;
/// 两侧带空格的分隔符。它在场时优先于裸连字符，见 [`split_parts`]。
const SPACED_DASH: &str = " - ";

/// 目录给出的提示，父目录当专辑、祖父目录当艺术家。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirHints<'a> {
    pub album: Option<&'a str>,
    pub artist: Option<&'a str>,
}

/// 解析一个目录时共用的上下文，目录内每一个文件都要用它：
/// 「哪一侧重复」这一信号无法由单个文件得出。
#[derive(Clone, Copy, Debug, Default)]
pub struct FolderContext<'a> {
    /// 库内已知艺术家，键是 [`artist_key`] 折过的形式。
    pub known_artists: Option<&'a HashSet<String>>,
    /// 同目录其它文件给出的重复统计，见 [`Siblings`]。
    pub siblings: Option<&'a Siblings>,
}

/// 推断结果。`title` 永远非空；`artist` / `album` 是有把握才给。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guess {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// 名字的比较形式。全角折半角、各种破折号统一、压掉多余空白，再做小写。
/// 落盘的已知艺人集合与这里的所有比较都必须走它，否则全角半角会被当成两个人。
pub fn artist_key(name: &str) -> String {
    normalise(name).to_lowercase()
}

/// 从路径推目录提示。
///
/// `Music`、`下载`、`新建文件夹` 这类泛用目录名不作为提示。它们表达的是「此处存放音乐」，
/// 而非「此专辑的名称」。宁可舍弃一层信息，也不构造一批名为「Music」的专辑。
///
/// 「第几张碟」这类中间层需要向上跨越。`周杰伦/七里香/Disc 1/` 中，
/// 专辑为七里香、艺术家为周杰伦，`Disc 1` 两者皆非。
pub fn hints_for(path: &Path) -> DirHints<'_> {
    let mut found: Vec<&str> = Vec::with_capacity(2);
    let mut cursor = path.parent();
    while let Some(dir) = cursor {
        if let Some(name) = dir.file_name().and_then(|name| name.to_str()) {
            if is_generic_dir(name) {
                break;
            }
            if !is_part_dir(name) {
                found.push(name);
                if found.len() == 2 {
                    break;
                }
            }
        }
        cursor = dir.parent();
    }

    DirHints {
        album: found.first().copied(),
        artist: found.get(1).copied(),
    }
}

/// 同一目录里各文件「首段与次段」的出现次数。
///
/// 专辑目录里十首曲目的艺术家相同，标题各不相同，于是首段重复、次段不重复。
/// 反过来 `歌名 - 歌手` 的目录里次段重复。这条判断不依赖任何词典，纯靠统计，
/// 是整张表里唯一能自动适应「压制者习惯」的信号。
#[derive(Clone, Debug, Default)]
pub struct Siblings {
    heads: HashMap<String, usize>,
    tails: HashMap<String, usize>,
}

impl Siblings {
    /// 收下一个目录里的全部文件名，不含扩展名。
    pub fn of<'a>(stems: impl IntoIterator<Item = &'a str>) -> Self {
        let mut siblings = Self::default();
        for stem in stems {
            let parts = parse(stem).parts;
            let Some(head) = parts.first() else { continue };
            *siblings.heads.entry(artist_key(head)).or_default() += 1;
            if let Some(tail) = parts.get(1) {
                *siblings.tails.entry(artist_key(tail)).or_default() += 1;
            }
        }
        siblings
    }

    /// 至少另有一个同目录文件也以它作为首段与次段；计数 ≥ 2 表示本文件之外仍有人使用该段。
    fn repeats(&self, name: &str) -> (bool, bool) {
        let key = artist_key(name);
        (
            self.heads.get(&key).is_some_and(|seen| *seen >= 2),
            self.tails.get(&key).is_some_and(|seen| *seen >= 2),
        )
    }
}

/// 解析目录，把同一份 [`Siblings`] 供给目录内的每一个文件。
pub fn guess_folder(
    stems: &[String],
    dirs: DirHints<'_>,
    folder: &FolderContext<'_>,
) -> Vec<Option<Guess>> {
    let siblings = Siblings::of(stems.iter().map(String::as_str));
    let scoped = FolderContext {
        siblings: Some(&siblings),
        ..*folder
    };
    stems
        .iter()
        .map(|stem| guess(stem, dirs, &scoped))
        .collect()
}

/// 推断一首。`None` 表示未作任何改动，调用方应保留原样，即以整串作为标题。
pub fn guess(stem: &str, dirs: DirHints<'_>, folder: &FolderContext<'_>) -> Option<Guess> {
    let parsed = parse(stem);
    let Some(head) = parsed.parts.first() else {
        // 剥除括号与音轨号后可能什么都不剩，如文件名仅为 `01`，不得把标题剥空。
        return None;
    };
    debug_assert!(!head.is_empty());

    let mut artist_hint: Option<String> = None;
    let mut album_hint: Option<String> = None;
    let mut book_hint: Option<String> = None;
    let mut suffixes: Vec<String> = Vec::new();

    for (kind, text) in &parsed.brackets {
        match role_of(*kind, text, dirs, folder) {
            Role::Artist => {
                if artist_hint.is_none() {
                    artist_hint = Some(text.clone());
                }
            }
            Role::Album => {
                if album_hint.is_none() {
                    album_hint = Some(text.clone());
                    if *kind == Bracket::Book {
                        book_hint = Some(text.clone());
                    }
                }
            }
            Role::Suffix => suffixes.push(text.clone()),
            Role::TrackNo | Role::Year | Role::Junk => {}
        }
    }

    let clean = decorate(&parsed.parts.join(" - "), &suffixes);
    let candidates = candidates(
        &parsed.parts,
        artist_hint.as_deref(),
        album_hint.as_deref(),
        book_hint.as_deref(),
    );
    if candidates.is_empty() {
        // 只剩一段正文且无括号信息，也可能只是完成了清洗，即去掉音轨号或压制组前缀。
        // 洗干净了才值得回报，否则一个字都没变，回报 `None` 让上层保留原样。
        if clean == normalise(stem) {
            return None;
        }
        return Some(Guess {
            title: clean,
            artist: None,
            album: None,
        });
    }

    let mut scored: Vec<(i32, usize)> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| (score(candidate, dirs, folder), index))
        .collect();
    scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));

    let &(best, winner) = scored.first()?;
    // 只有一个候选时没有第二名可比，领先量直接算作充足。
    let decisive = scored.get(1).is_none_or(|second| best - second.0 >= MARGIN);
    if best < ACCEPT || !decisive {
        return None;
    }

    let candidate = &candidates[winner];
    Some(Guess {
        title: decorate(&candidate.title, &suffixes),
        artist: candidate.artist.clone(),
        album: candidate.album.clone(),
    })
}

/// 一轮候选，同一次「角色分配」。
struct Candidate {
    artist: Option<String>,
    album: Option<String>,
    title: String,
    /// 位置先验带来的底分。
    prior: i32,
    /// `artist` 是否来自括号里点名的名字。
    from_bracket: bool,
}

fn candidates(
    parts: &[String],
    artist_hint: Option<&str>,
    album_hint: Option<&str>,
    book: Option<&str>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    let artist_hint = artist_hint.map(ToOwned::to_owned);
    let album_hint = album_hint.map(ToOwned::to_owned);

    // 括号里点名了艺术家或专辑时，正文整体当标题本身就是一种候选：
    // `【周杰伦】七里香` 只有一段正文，靠的就是这一条。
    if artist_hint.is_some() || album_hint.is_some() {
        out.push(Candidate {
            artist: artist_hint.clone(),
            album: album_hint.clone(),
            title: parts.join(" - "),
            prior: 0,
            from_bracket: artist_hint.is_some(),
        });
    }

    if parts.len() >= 2 {
        out.push(Candidate {
            artist: Some(parts[0].clone()),
            album: None,
            title: parts[1..].join(" - "),
            prior: PRIOR_ARTIST_FIRST,
            from_bracket: false,
        });
        out.push(Candidate {
            artist: Some(parts[parts.len() - 1].clone()),
            album: None,
            title: parts[..parts.len() - 1].join(" - "),
            prior: 0,
            from_bracket: false,
        });
    }

    if parts.len() >= 3 {
        // 中间那段只有在目录确认它确实是专辑时才该被单独摘出来，所以底分低一分：
        // `久石让 - 天空之城 - 钢琴版` 里的「天空之城」更可能是标题的一部分。
        out.push(Candidate {
            artist: Some(parts[0].clone()),
            album: Some(parts[1].clone()),
            title: parts[2..].join(" - "),
            prior: PRIOR_ARTIST_FIRST - 1,
            from_bracket: false,
        });
    }

    // `周杰伦《七里香》`。书名号里是作品名，前面那一段是艺术家。
    // 不把书名号里的内容同时当作专辑，因为只有一首曲目的「专辑」没有意义，
    // 专辑该由目录给，目录没给就留空。
    if let (1, Some(book)) = (parts.len(), book) {
        out.push(Candidate {
            artist: Some(parts[0].clone()),
            album: None,
            title: book.to_owned(),
            prior: PRIOR_ARTIST_FIRST,
            from_bracket: false,
        });
    }

    dedupe(out)
}

/// 同一条分配可能被不同规则重复生成，如 `A - A`，应去重，避免同一分配相互竞争。
fn dedupe(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let duplicate = out.iter().any(|kept| {
            kept.artist == candidate.artist
                && kept.album == candidate.album
                && kept.title == candidate.title
        });
        if !duplicate {
            out.push(candidate);
        }
    }
    out
}

fn score(candidate: &Candidate, dirs: DirHints<'_>, folder: &FolderContext<'_>) -> i32 {
    let mut score = candidate.prior;

    if let Some(artist) = &candidate.artist {
        // 同目录内的重复统计优先于位置先验，重复出现者即为艺术家。
        if let Some(siblings) = folder.siblings {
            let (head, tail) = siblings.repeats(artist);
            let (title_head, title_tail) = siblings.repeats(&candidate.title);
            let artist_repeats = head || tail;
            let title_repeats = title_head || title_tail;
            if artist_repeats && !title_repeats {
                score += 4;
            } else if title_repeats && !artist_repeats {
                score -= 4;
            }
        }

        // 祖父目录与库内词典点名，仅次于同目录一致性。
        if matches_dir(dirs.artist, artist) {
            score += 4;
        }
        if folder
            .known_artists
            .is_some_and(|known| known.contains(&artist_key(artist)))
        {
            score += 3;
        }
        if candidate.from_bracket {
            score += 3;
        }

        // 专辑名当成了艺术家，说明切反了，或者把「专辑 - 曲目」当成了「歌手 - 歌名」
        if matches_dir(dirs.album, artist) {
            score -= 6;
        }
        // 版本标记出现在艺术家侧，说明切反了，如 `七里香(Live) - 周杰伦`。
        if has_title_marker(artist) {
            score -= 6;
        }
        // 纯数字不能作为名字，如 `1 - Intro` 中的 1。
        if artist.chars().all(|ch| ch.is_ascii_digit()) {
            score -= 6;
        }
        if artist.chars().count() > 40 {
            score -= 4;
        }
    }

    if let Some(album) = &candidate.album {
        // 从正文里额外摘出来的专辑，要目录点头才算数。
        score += if matches_dir(dirs.album, album) {
            3
        } else {
            -1
        };
    }

    score
}

/// 候选成立时把版本标记接回标题，`七里香` + `(Live)` → `七里香 (Live)`。
fn decorate(title: &str, suffixes: &[String]) -> String {
    let mut out = title.to_owned();
    for suffix in suffixes {
        out.push_str(" (");
        out.push_str(suffix);
        out.push(')');
    }
    out
}

// ---------------------------------------------------------------- 切分。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bracket {
    Round,
    Square,
    Book,
}

/// 切分结果是正文段与括号内容。
#[derive(Debug, Default)]
struct Parsed {
    parts: Vec<String>,
    brackets: Vec<(Bracket, String)>,
}

fn parse(raw: &str) -> Parsed {
    let text = normalise(raw);
    let mut body = String::with_capacity(text.len());
    let mut brackets: Vec<(Bracket, String)> = Vec::new();
    let mut open: Option<(char, Bracket, String)> = None;

    for ch in text.chars() {
        match open.take() {
            None => match bracket_pair(ch) {
                Some((close, kind)) => {
                    open = Some((close, kind, String::new()));
                    // 括号两侧断成两段，免得 `周杰伦《七里香》` 粘成一个词。
                    body.push(' ');
                }
                None => body.push(ch),
            },
            Some((close, kind, mut content)) => {
                if ch == close {
                    brackets.push((kind, content.trim().to_owned()));
                    body.push(' ');
                } else {
                    if content.len() < MAX_BRACKET {
                        content.push(ch);
                    }
                    open = Some((close, kind, content));
                }
            }
        }
    }
    // 没闭合的括号按已闭合处理，内容照样收下；正文里那一截本来就已经断开了。
    if let Some((_, kind, content)) = open {
        brackets.push((kind, content.trim().to_owned()));
    }

    let (_, rest) = strip_track_no(&body);
    Parsed {
        parts: split_parts(rest),
        brackets,
    }
}

fn bracket_pair(ch: char) -> Option<(char, Bracket)> {
    match ch {
        '(' => Some((')', Bracket::Round)),
        '〔' => Some(('〕', Bracket::Round)),
        '[' => Some((']', Bracket::Square)),
        '【' => Some(('】', Bracket::Square)),
        '〖' => Some(('〗', Bracket::Square)),
        '《' => Some(('》', Bracket::Book)),
        '〈' => Some(('〉', Bracket::Book)),
        _ => None,
    }
}

/// 按分隔符切成正文段。
///
/// 「空格 - 空格」是最无歧义的分隔符，出现时只认它。名字与标题自带连字符的情况很常见，
/// 如 `塞壬唱片-MSR,z1on,ReStudio`、`Secret of my heart-Hi-Res-5.1`；按裸 `-` 切会将其切断。
fn split_parts(body: &str) -> Vec<String> {
    if body.contains(SPACED_DASH) {
        body.split(SPACED_DASH).filter_map(trimmed).collect()
    } else {
        body.split(['-', '|', '_']).filter_map(trimmed).collect()
    }
}

fn trimmed(part: &str) -> Option<String> {
    let part = part.trim();
    if part.is_empty() {
        None
    } else {
        Some(part.to_owned())
    }
}

/// 剥掉前导音轨号，返回音轨号与余下正文；判不出时原样返回。
///
/// 三道闸分别是数字最多三位、后面必须紧跟分隔符，以及空格分隔时数字要带前导零。
/// `2002年的第一场雪` 于是不会被削掉四位数字；`7 rings` 里那个孤零零的 7 也不算。
/// 它更像标题的一部分，而不是第 7 轨。
fn strip_track_no(body: &str) -> (Option<u32>, &str) {
    let text = body.trim_start();
    let text = strip_prefix(text);

    let digits = text.chars().take_while(char::is_ascii_digit).count();
    if !(1..=3).contains(&digits) {
        return (None, body);
    }
    let (number, rest) = text.split_at(digits);

    let Some(separator) = rest.chars().next().filter(|ch| is_separator(*ch)) else {
        return (None, body);
    };
    if separator == ' ' && !number.starts_with('0') {
        return (None, body);
    }
    let rest = rest.trim_start_matches(is_separator).trim_start();
    // 只剩一个数字时不能剥，`01.flac` 的标题就是「01」
    if rest.is_empty() {
        return (None, body);
    }
    match number.parse::<u32>() {
        Ok(no) => (Some(no), rest),
        Err(_) => (None, body),
    }
}

/// `track 01` / `no. 01` 这类前缀；没有就原样返回。
fn strip_prefix(text: &str) -> &str {
    for prefix in ["track", "no.", "no ", "音轨"] {
        let matched = text
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix));
        if matched {
            return text[prefix.len()..].trim_start();
        }
    }
    text
}

fn is_separator(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '.' | '、' | '-' | '_' | '|' | ')' | ']' | ':' | '．'
    )
}

// ---------------------------------------------------------------- 括号含义。

enum Role {
    Artist,
    Album,
    Suffix,
    TrackNo,
    Year,
    Junk,
}

fn role_of(kind: Bracket, text: &str, dirs: DirHints<'_>, folder: &FolderContext<'_>) -> Role {
    if text.is_empty() {
        return Role::Junk;
    }

    // 纯数字时，短的是音轨号，四位的是年份。
    if text.chars().all(|ch| ch.is_ascii_digit()) {
        if text.len() <= 3 {
            return Role::TrackNo;
        }
        if text.len() == 4 {
            return Role::Year;
        }
        return Role::Junk;
    }

    let key = artist_key(text);
    if has_junk_marker(&key) {
        return Role::Junk;
    }
    if has_version_marker(&key) {
        return Role::Suffix;
    }

    // 目录与词典点名优先于括号形状。
    if matches_dir(dirs.album, text) {
        return Role::Album;
    }
    if matches_dir(dirs.artist, text)
        || folder
            .known_artists
            .is_some_and(|known| known.contains(&key))
    {
        return Role::Artist;
    }

    match kind {
        // 书名号在中文语境下表示作品名，即专辑名或曲名。
        Bracket::Book => Role::Album,
        // 方头括号多数是压制组、论坛名，没被上面接住的就丢掉。
        Bracket::Square => {
            if is_cjk_name(text) {
                Role::Artist
            } else {
                Role::Junk
            }
        }
        // 圆括号里是「(周杰伦)」还是「(Live)」只能看内容。纯中文的短名字当艺术家，
        // 其余一律接回标题。判断错误只会让标题多一个括号，丢弃则会导致信息永久丢失。
        Bracket::Round => {
            if is_cjk_name(text) {
                Role::Artist
            } else {
                Role::Suffix
            }
        }
    }
}

/// 纯非 ASCII 的短名字，如 `周杰伦`、`坂本龍一`。像个人的名字才敢往艺术家上押。
fn is_cjk_name(text: &str) -> bool {
    let count = text.chars().count();
    (2..=10).contains(&count)
        && text
            .chars()
            .all(|ch| !ch.is_ascii() && !ch.is_whitespace() && !ch.is_numeric())
}

// ---------------------------------------------------------------- 文本归一。

/// 折成便于比较的形式，即全角转半角、各种破折号统一成 `-`、压掉多余空白。
fn normalise(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut space = false;
    for ch in raw.chars() {
        let ch = match ch {
            '\u{3000}' => ' ',
            // 全角标点与全角数字都落在这一段全角 ASCII 里。
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(u32::from(ch) - 0xFEE0).unwrap_or(ch),
            // 连字符、en/em dash、减号，写法有十几种，含义只有一种。
            '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE58}' | '\u{FE63}' => '-',
            // 零宽字符，常见于从网页上抄下来的文件名。
            '\u{200B}'..='\u{200D}' | '\u{FEFF}' => continue,
            _ => ch,
        };
        if ch.is_whitespace() {
            if !space {
                out.push(' ');
                space = true;
            }
        } else {
            out.push(ch);
            space = false;
        }
    }
    out.trim().to_owned()
}

// ---------------------------------------------------------------- 标记词。

/// 出现在标题侧很正常的词。出现在艺术家侧就说明切反了。
const VERSION_MARKERS: &[&str] = &[
    "live",
    "remix",
    "acoustic",
    "demo",
    "instrumental",
    "inst.",
    "off vocal",
    "cover",
    "edit",
    "mix",
    "version",
    "ver.",
    "feat",
    "ft.",
    "伴奏",
    "纯音乐",
    "现场",
    "翻唱",
    "片段",
    "演唱会",
    "版",
];

/// 只可能是噪声的词，如压制组、论坛、码率、格式。括号内容若属于这些类别则整段丢弃。
const JUNK_MARKERS: &[&str] = &[
    "www.", "http", "://", ".com", ".cn", "论坛", "压制", "首发", "整理", "分享", "kbps", "320k",
    "128k", "flac", "mp3", "wav", "dsd", "hires", "hi-res",
];

fn has_version_marker(folded: &str) -> bool {
    contains_any(folded, VERSION_MARKERS)
}

fn has_title_marker(text: &str) -> bool {
    contains_any(&artist_key(text), VERSION_MARKERS)
}

fn has_junk_marker(folded: &str) -> bool {
    contains_any(folded, JUNK_MARKERS)
}

/// `folded` 是已经折过小写的文本。
///
/// ASCII 标记要求词边界，否则 `live` 会把 `Alive` 也命中；中文标记按子串算，
/// 因为中文里没有词边界这回事。
fn contains_any(folded: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| {
        if marker.is_ascii() {
            folded.match_indices(marker).any(|(at, _)| {
                let before = folded[..at].chars().next_back();
                let after = folded[at + marker.len()..].chars().next();
                before.is_none_or(|ch| !ch.is_alphanumeric())
                    && after.is_none_or(|ch| !ch.is_alphanumeric())
            })
        } else {
            folded.contains(marker)
        }
    })
}

fn matches_dir(hint: Option<&str>, value: &str) -> bool {
    hint.is_some_and(|hint| artist_key(hint) == artist_key(value))
}

/// 这些话只说明「这里放音乐」，不能当专辑名或艺术家名用。
const GENERIC_DIRS: &[&str] = &[
    "music",
    "musics",
    "我的音乐",
    "音乐",
    "歌曲",
    "music library",
    "songs",
    "audio",
    "音频",
    "媒体",
    "media",
    "download",
    "downloads",
    "下载",
    "待整理",
    "未整理",
    "新建文件夹",
    "new folder",
    "mp3",
    "flac",
    "无损",
    "lossless",
    "desktop",
    "桌面",
    "documents",
    "文档",
    "onedrive",
    "dropbox",
    "baidunetdisk",
    "百度网盘",
    "temp",
    "tmp",
];

fn is_generic_dir(name: &str) -> bool {
    let folded = name.trim().to_lowercase();
    folded.is_empty() || GENERIC_DIRS.contains(&folded.as_str())
}

/// 「第几张碟」这类中间层。`Disc 1`、`CD2`、`Vol.1`。它们既不是专辑也不是艺术家，
/// 推断目录提示时要跨过去。
fn is_part_dir(name: &str) -> bool {
    let folded: String = name
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '.' && *ch != '_' && *ch != '-')
        .flat_map(char::to_lowercase)
        .collect();

    ["disc", "disk", "cd", "vol", "volume", "part"]
        .iter()
        .any(|prefix| {
            folded
                .strip_prefix(prefix)
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| artist_key(name)).collect()
    }

    fn plain() -> FolderContext<'static> {
        FolderContext::default()
    }

    fn with_artists(artists: &HashSet<String>) -> FolderContext<'_> {
        FolderContext {
            known_artists: Some(artists),
            siblings: None,
        }
    }

    fn guess_plain(stem: &str) -> Option<Guess> {
        guess(stem, DirHints::default(), &plain())
    }

    #[test]
    fn 歌手在前的常规写法能切开() {
        let found = guess_plain("周杰伦 - 七里香").expect("两段式应当切得开");
        assert_eq!(found.artist.as_deref(), Some("周杰伦"));
        assert_eq!(found.title, "七里香");
    }

    #[test]
    fn 只有一段正文时一个字都不动() {
        assert!(
            guess_plain("纸飞机与旧磁带").is_none(),
            "看不出谁是艺术家就不该硬切"
        );
    }

    /// 用户最常抱怨的那种。`歌名 - 歌手` 与 `歌手 - 歌名` 长得一模一样，
    /// 只能靠目录里重复出现的那一侧来定方向。
    #[test]
    fn 目录里重复的那一侧才是艺术家() {
        let artists = known(&["周杰伦", "陈奕迅"]);
        let stems: Vec<String> = ["七里香 - 周杰伦", "十年 - 陈奕迅", "富士山下 - 陈奕迅"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let folder = FolderContext {
            known_artists: Some(&artists),
            siblings: None,
        };
        let found = guess_folder(&stems, DirHints::default(), &folder);

        let first = found[0].as_ref().expect("应当切开");
        assert_eq!(first.artist.as_deref(), Some("周杰伦"));
        assert_eq!(first.title, "七里香");

        let third = found[2].as_ref().expect("应当切开");
        assert_eq!(
            third.artist.as_deref(),
            Some("陈奕迅"),
            "重复出现的才是艺术家"
        );
        assert_eq!(third.title, "富士山下");
    }

    #[test]
    fn 祖父目录点名时反而能纠正方向() {
        let artists = known(&["周杰伦"]);
        let dirs = DirHints {
            album: Some("七里香"),
            artist: Some("周杰伦"),
        };
        let found = guess("七里香 - 周杰伦", dirs, &with_artists(&artists)).expect("应当切开");
        assert_eq!(found.artist.as_deref(), Some("周杰伦"));
        assert_eq!(found.title, "七里香");

        // 正着写也一样，专辑名当艺术家要被 -6 压下去。
        let found = guess("周杰伦 - 七里香", dirs, &with_artists(&artists)).expect("应当切开");
        assert_eq!(found.artist.as_deref(), Some("周杰伦"));
    }

    #[test]
    fn 全角与伪装的分隔符都折得回来() {
        let found = guess_plain("０１．周杰伦 － 七里香").expect("全角也要能切");
        assert_eq!(found.artist.as_deref(), Some("周杰伦"));
        assert_eq!(found.title, "七里香", "音轨号不该留在标题里");
    }

    #[test]
    fn 数字开头的标题不会被当成音轨号() {
        for stem in ["2002年的第一场雪", "7 rings", "1989", "24K Magic"] {
            let found = guess_plain(stem);
            assert!(
                found.as_ref().is_none_or(|g| g.title == stem),
                "《{stem}》的标题被削了：{found:?}"
            );
        }
    }

    #[test]
    fn 音轨号后面没有分隔符就不算音轨号() {
        let (no, rest) = strip_track_no("01七里香");
        assert_eq!(no, None);
        assert_eq!(rest, "01七里香", "没分隔符说明这串数字就是标题的一部分");
    }

    #[test]
    fn 方头括号里的歌手能认出来() {
        let found = guess_plain("【周杰伦】七里香").expect("括号点名了就该切");
        assert_eq!(found.artist.as_deref(), Some("周杰伦"));
        assert_eq!(found.title, "七里香");
    }

    #[test]
    fn 压制组前缀整段丢掉() {
        let found = guess_plain("[某某音乐论坛] 周杰伦 - 七里香").expect("应当切开");
        assert_eq!(found.artist.as_deref(), Some("周杰伦"));
        assert_eq!(found.title, "七里香", "论坛名不该混进标题：{}", found.title);
    }

    #[test]
    fn 版本标记接回标题而不是当艺术家() {
        let found = guess_plain("周杰伦 - 七里香 (Live)").expect("应当切开");
        assert_eq!(found.artist.as_deref(), Some("周杰伦"), "Live 不是艺术家");
        assert_eq!(found.title, "七里香 (Live)", "版本信息要留着");
    }

    #[test]
    fn 三段式把多出来的那段留在标题里() {
        let found = guess_plain("久石让 - 天空之城 - 钢琴版").expect("应当切开");
        assert_eq!(found.artist.as_deref(), Some("久石让"));
        assert_eq!(found.title, "天空之城 - 钢琴版");
    }

    /// 名字和标题自带的连字符不该按分隔符处理，有「空格 - 空格」时只认它。
    #[test]
    fn 带空格的连字符优先于裸连字符() {
        let found = guess_plain("塞壬唱片-MSR,z1on,ReStudio - 布道人").expect("应当切开");
        assert_eq!(
            found.artist.as_deref(),
            Some("塞壬唱片-MSR,z1on,ReStudio"),
            "名字里的连字符被切碎了"
        );
        assert_eq!(found.title, "布道人");

        let found = guess_plain("棗いつき - Secret of my heart-Hi-Res-5.1").expect("应当切开");
        assert_eq!(found.artist.as_deref(), Some("棗いつき"));
        assert_eq!(
            found.title, "Secret of my heart-Hi-Res-5.1",
            "标题里的连字符应当原样留着"
        );
    }

    /// 没有带空格的分隔符时，裸连字符仍然是分隔符。
    #[test]
    fn 裸连字符照旧当分隔符() {
        let found = guess_plain("MyGO!!!!!-春日影").expect("应当切开");
        assert_eq!(found.artist.as_deref(), Some("MyGO!!!!!"));
        assert_eq!(found.title, "春日影");
    }

    #[test]
    fn 书名号里是作品名() {
        let found = guess_plain("周杰伦《七里香》").expect("应当切开");
        assert_eq!(found.artist.as_deref(), Some("周杰伦"));
        assert_eq!(found.title, "七里香");
    }

    #[test]
    fn 泛用目录名不当提示() {
        assert_eq!(
            hints_for(Path::new("/music/下载/新建文件夹/x.flac")),
            DirHints::default(),
            "这几层说的都是「这里放音乐」，不是名字"
        );
        assert_eq!(
            hints_for(Path::new("/music/周杰伦/七里香/01. 七里香.flac")),
            DirHints {
                album: Some("七里香"),
                artist: Some("周杰伦")
            }
        );
    }

    #[test]
    fn 目录提示只认目录名不认路径前缀() {
        let dirs = hints_for(Path::new("/music/周杰伦/七里香/live/x.flac"));
        assert_eq!(dirs.album, Some("live"), "真目录名照收，不做语义过滤");
        assert_eq!(dirs.artist, Some("七里香"));
    }

    /// 多碟专辑里 `Disc 1` 谁都不是，往上一层才是专辑名。
    #[test]
    fn 碟号那一层不算专辑名() {
        for path in [
            "/music/周杰伦/七里香/Disc 1/x.flac",
            "/music/周杰伦/七里香/CD2/x.flac",
            "/music/周杰伦/七里香/vol.1/x.flac",
        ] {
            let dirs = hints_for(Path::new(path));
            assert_eq!(dirs.album, Some("七里香"), "{path} 把碟号当成了专辑");
            assert_eq!(dirs.artist, Some("周杰伦"), "{path} 的艺术家错了");
        }
    }

    #[test]
    fn 名字比较不吃全角与大小写() {
        assert_eq!(artist_key("ＡＢＢＡ"), artist_key("abba"));
        assert_eq!(artist_key(" 陈屿 "), artist_key("陈屿"));
        assert_ne!(artist_key("陈屿"), artist_key("陈与"));
    }

    /// 标记词必须要求词边界，否则 `Alive` 会被 `live` 命中，
    /// 一首叫《Alive》的歌会被判成现场版。
    #[test]
    fn ascii标记词要卡词边界() {
        assert!(has_version_marker("live version"));
        assert!(has_version_marker("(live)"));
        assert!(!has_version_marker("alive"), "Alive 不是现场版");
        assert!(!has_version_marker("olivia"));
        assert!(has_version_marker("钢琴版"), "中文按子串算");
    }

    #[test]
    fn 纯数字当不了艺术家() {
        // 如 `1 - Intro`，数字被当成音轨号剥掉之后不该反过来变成艺术家。
        let found = guess_plain("1 - Intro");
        assert!(
            found.as_ref().is_none_or(|g| g.artist.is_none()),
            "把序号当成了艺术家：{found:?}"
        );
    }

    #[test]
    fn 剥完什么都不剩时不动原样() {
        assert!(guess_plain("01").is_none());
        assert!(guess_plain("[]").is_none());
    }
}
