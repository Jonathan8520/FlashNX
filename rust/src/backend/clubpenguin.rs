//! Puffle Launch and DJ3000 save again (#134), and Pufflescape keeps its scores.
//!
//! Club Penguin's minigames saved through the game they ran in: the Club
//! Penguin client (`_global.getCurrentShell()`) and its connection to the
//! server (`_global.getCurrentAirtower()`, messages "sgd" to save, "ggd" to
//! load). Flashpoint plays them alone, where neither exists, so nothing was
//! ever kept. They are the only two of its 110 Club Penguin games that save
//! that way. Flashpoint's Puffle Launch, the copy Disney's game site served
//! alone, even switches its save off on its second frame (`saveDataOn =
//! false;`, then the load and every save are skipped).
//!
//! For those games only, the movie is changed as it loads:
//! - the AS2 in `clubpenguin_shell.as` (compiled to `clubpenguin_shell.bin`)
//!   runs first on frame 1 and answers as the client and the server do
//!   (Houdini's answers, the real client's listeners): a member, as both
//!   games save for members only, and "sgd"/"ggd" kept by slot in a
//!   SharedObject, so a `.sol` on the card like any other save;
//! - `saveDataOn = false` becomes `true`, found by reading the frame scripts'
//!   actions (a push of that name and `false`, then SetVariable), not at a
//!   fixed place, so both language copies of Puffle Launch get it.
//!
//! Everything else they ask of the client (music, stamps, furniture, the
//! penguin's colour) stays unanswered, as it is today without a client: those
//! calls did nothing. Only these games: 47 of Flashpoint's Club Penguin games
//! look for the client, and finding one changes what they do.
//!
//! Pufflescape is an AS3 game started by a host movie, `HostEmulator.swf`
//! (Club Penguin's 2011 test host, which Flashpoint launches): the game asks
//! it which stamps the player has, to open its levels, and kept nothing of
//! its own, not even on Club Penguin. Its O-berries, rare berry and time per
//! level lived on the level select's icons and were gone at the next launch.
//! That host is replaced by `pufflescape_host/` (compiled to
//! `pufflescape_host.bin`): the same answers, every stamp earned as before,
//! and the icons' scores kept in a SharedObject and put back on them.

const SHIM: &[u8] = include_bytes!("clubpenguin_shell.bin");

/// FlashNX's Pufflescape host: `pufflescape_host/HostEmulator.as` and
/// `CPHostEmulator.as` compiled into the original host movie, so its frame,
/// stage size and frame rate are the same. To rebuild it, with JPEXS FFDec:
///   ffdec-cli -replace HostEmulator.swf out.swf com.clubpenguin.CPHostEmulator CPHostEmulator.as HostEmulator HostEmulator.as
/// where HostEmulator.swf is Flashpoint's
/// (media1.clubpenguin.com/play/v2/games/pufflescape/HostEmulator.swf).
const PUFFLESCAPE_HOST: &[u8] = include_bytes!("pufflescape_host.bin");

/// The games this applies to, by their movie's address.
fn is_listed(url: &str) -> bool {
    let url = url.to_ascii_lowercase();
    // Puffle Launch (both languages), DJ3000.
    url.contains("/clp_spl_puz_pufflelaunch/cp_pufflelaunch.swf") || url.contains("/games/mixmaster/choosesong.swf")
}

/// The movie to play for `url`: `None` to play `swf` as it is.
pub fn prepare(url: &str, swf: &[u8]) -> Option<Vec<u8>> {
    if url.to_ascii_lowercase().contains("/games/pufflescape/hostemulator.swf") {
        tracing::info!("clubpenguin: FlashNX's host given to {url}, scores kept");
        return Some(PUFFLESCAPE_HOST.to_vec());
    }
    if !is_listed(url) || swf.len() < 8 {
        return None;
    }
    let mut data = match &swf[0..3] {
        b"FWS" => swf.to_vec(),
        b"CWS" => {
            use std::io::Read;
            let mut out = swf[0..8].to_vec();
            out[0] = b'F';
            flate2::read::ZlibDecoder::new(&swf[8..]).read_to_end(&mut out).ok()?;
            out
        }
        _ => return None,
    };
    // The frame rect is bit-packed: 5 bits of size, then four values of it.
    let nbits = (*data.get(8)? >> 3) as usize;
    let first_tag = 8 + (5 + 4 * nbits).div_ceil(8) + 4;
    let end = data.len();
    let flipped = flip_save_switch(&mut data, first_tag, end);
    // Frame 1 starts with FileAttributes (and maybe Metadata): after them.
    let mut at = first_tag;
    while let Some((code, _, next)) = tag_at(&data, at) {
        if code != 69 && code != 77 {
            break;
        }
        at = next;
    }
    let mut tag = Vec::with_capacity(SHIM.len() + 6);
    tag.extend_from_slice(&((12u16 << 6) | 0x3F).to_le_bytes());
    tag.extend_from_slice(&(SHIM.len() as u32).to_le_bytes());
    tag.extend_from_slice(SHIM);
    data.splice(at..at, tag);
    let len = data.len() as u32;
    data[4..8].copy_from_slice(&len.to_le_bytes());
    tracing::info!("clubpenguin: shell and server given to {url}, save switched on {flipped} time(s)");
    Some(data)
}

/// The tag at `at`: (code, body range start, next tag), `None` past the end.
fn tag_at(data: &[u8], at: usize) -> Option<(u16, usize, usize)> {
    let head = u16::from_le_bytes([*data.get(at)?, *data.get(at + 1)?]);
    let code = head >> 6;
    let (len, body) = if head & 0x3F == 0x3F {
        let l = u32::from_le_bytes(data.get(at + 2..at + 6)?.try_into().ok()?) as usize;
        (l, at + 6)
    } else {
        ((head & 0x3F) as usize, at + 2)
    };
    let next = body.checked_add(len)?;
    (code != 0 && next <= data.len()).then_some((code, body, next))
}

/// Every `saveDataOn = false` of the tags in `start..end` (and in the sprites
/// they define) made `true`. Returns how many.
fn flip_save_switch(data: &mut [u8], start: usize, end: usize) -> usize {
    let mut flipped = 0;
    let mut at = start;
    while at < end {
        let Some((code, body, next)) = tag_at(data, at) else {
            break;
        };
        match code {
            // DoAction
            12 => flipped += flip_in_actions(data, body, next),
            // DoInitAction: a sprite id, then actions
            59 => flipped += flip_in_actions(data, body + 2, next),
            // DefineSprite: id, frame count, then its own tags
            39 => flipped += flip_save_switch(data, body + 4, next),
            _ => {}
        }
        at = next;
    }
    flipped
}

/// One action stream: a push of the name `saveDataOn` (inline, or from the
/// constant pool) and the boolean `false`, then SetVariable. Function bodies
/// follow their definitions inline, so a straight walk reads them too.
fn flip_in_actions(data: &mut [u8], start: usize, end: usize) -> usize {
    let mut pool: Vec<Vec<u8>> = Vec::new();
    let mut flipped = 0;
    let mut at = start;
    while at < end {
        let op = data[at];
        if op == 0 {
            break;
        }
        if op < 0x80 {
            at += 1;
            continue;
        }
        let Some(len) = data.get(at + 1..at + 3).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize) else {
            break;
        };
        let body = at + 3;
        let next = body + len;
        if next > end {
            break;
        }
        match op {
            0x88 if len >= 2 => {
                // ConstantPool: a count, then that many strings.
                pool.clear();
                let count = u16::from_le_bytes([data[body], data[body + 1]]) as usize;
                let mut p = body + 2;
                for _ in 0..count {
                    let Some(z) = data.get(p..next).and_then(|s| s.iter().position(|&c| c == 0)) else {
                        break;
                    };
                    pool.push(data[p..p + z].to_vec());
                    p += z + 1;
                }
            }
            0x96 => {
                if let Some(bool_at) = push_name_then_false(data, body, next, &pool) {
                    if data.get(next) == Some(&0x1D) {
                        data[bool_at] = 1;
                        flipped += 1;
                    }
                }
            }
            _ => {}
        }
        at = next;
    }
    flipped
}

/// Whether the push in `body..next` is exactly the name `saveDataOn` then
/// `false`; the place of the boolean's byte if so.
fn push_name_then_false(data: &[u8], body: usize, next: usize, pool: &[Vec<u8>]) -> Option<usize> {
    const NAME: &[u8] = b"saveDataOn";
    let push = data.get(body..next)?;
    let after_name = match *push.first()? {
        // a string
        0 => {
            let z = push.get(1..)?.iter().position(|&c| c == 0)?;
            (&push[1..1 + z] == NAME).then_some(2 + z)?
        }
        // a constant, by an index of one or two bytes
        8 => (pool.get(*push.get(1)? as usize)?.as_slice() == NAME).then_some(2)?,
        9 => {
            let i = u16::from_le_bytes([*push.get(1)?, *push.get(2)?]) as usize;
            (pool.get(i)?.as_slice() == NAME).then_some(3)?
        }
        _ => return None,
    };
    // then the boolean false, and nothing more in this push
    (push.get(after_name..)? == [5, 0]).then_some(body + after_name + 1)
}
