//! Copyright (c) 2015 Marc-Antoine Perennou
//! Copyright (c) 2022 nickelc
//! Copyright (c) 2023 Berrysoft

use nom::branch::alt;
use nom::bytes::complete::{tag, take, take_until};
use nom::character::complete::{digit1, oct_digit0, space0};
use nom::combinator::{all_consuming, iterator, map, map_parser, map_res};
use nom::error::ErrorKind;
use nom::multi::many0;
use nom::sequence::{pair, terminated};
use nom::*;
use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq)]
pub struct TarEntry<'a> {
    pub header: PosixHeader<'a>,
    pub contents: &'a [u8],
}

#[derive(Debug, PartialEq, Eq)]
pub struct PosixHeader<'a> {
    pub name: &'a str,
    pub mode: u64,
    pub uid: u64,
    pub gid: u64,
    pub size: u64,
    pub mtime: u64,
    pub chksum: &'a str,
    pub typeflag: TypeFlag,
    pub linkname: &'a str,
    pub ustar: ExtraHeader<'a>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeFlag {
    NormalFile,
    HardLink,
    SymbolicLink,
    CharacterSpecial,
    BlockSpecial,
    Directory,
    Fifo,
    ContiguousFile,
    PaxGlobal,
    Pax,
    GnuDirectory,
    GnuLongLink,
    GnuLongName,
    GnuSparse,
    GnuVolumeHeader,
    VendorSpecific,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExtraHeader<'a> {
    UStar(UStarHeader<'a>),
    Padding,
}

#[derive(Debug, PartialEq, Eq)]
pub struct UStarHeader<'a> {
    pub uname: &'a str,
    pub gname: &'a str,
    pub devmajor: u64,
    pub devminor: u64,
    pub extra: UStarExtraHeader<'a>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UStarExtraHeader<'a> {
    Posix(PosixExtraHeader<'a>),
    Gnu(GnuExtraHeader),
}

#[derive(Debug, PartialEq, Eq)]
pub struct PosixExtraHeader<'a> {
    pub prefix: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GnuExtraHeader {
    pub atime: u64,
    pub ctime: u64,
    pub offset: u64,
    pub sparses: Vec<Sparse>,
    pub realsize: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Sparse {
    pub offset: u64,
    pub numbytes: u64,
}

fn parse_bool(i: &[u8]) -> IResult<&[u8], bool> {
    map(take(1usize), |i: &[u8]| i[0] != 0)(i)
}

/// Read null-terminated string and ignore the rest
/// If there's no null, `size` will be the length of the string.
fn parse_str(size: usize) -> impl FnMut(&[u8]) -> IResult<&[u8], &str> {
    move |input| {
        let s = map_res(alt((take_until("\0"), take(size))), std::str::from_utf8);
        map_parser(take(size), s)(input)
    }
}

macro_rules! impl_parse_str {
    ($($name:ident, $size:expr;)+) => ($(
        fn $name(i: &[u8]) -> IResult<&[u8], &str> {
            parse_str($size)(i)
        }
    )+)
}

impl_parse_str! {
    parse_str8, 8;
    parse_str32, 32;
    parse_str100, 100;
    parse_str155, 155;
}

/// Octal string parsing
fn parse_octal(i: &[u8], n: usize) -> IResult<&[u8], u64> {
    let (rest, input) = take(n)(i)?;
    let (i, value) = terminated(oct_digit0, space0)(input)?;

    if i.input_len() == 0 || i[0] == 0 {
        let value = value
            .iter()
            .fold(0, |acc, v| acc * 8 + u64::from(*v - b'0'));
        Ok((rest, value))
    } else {
        Err(nom::Err::Error(error_position!(i, ErrorKind::OctDigit)))
    }
}

fn parse_octal8(i: &[u8]) -> IResult<&[u8], u64> {
    parse_octal(i, 8)
}

fn parse_octal12(i: &[u8]) -> IResult<&[u8], u64> {
    parse_octal(i, 12)
}

/// [`TypeFlag`] parsing
fn parse_type_flag(i: &[u8]) -> IResult<&[u8], TypeFlag> {
    let (c, rest) = match i.split_first() {
        Some((c, rest)) => (c, rest),
        None => return Err(nom::Err::Incomplete(Needed::new(1))),
    };
    let flag = match c {
        b'0' | b'\0' => TypeFlag::NormalFile,
        b'1' => TypeFlag::HardLink,
        b'2' => TypeFlag::SymbolicLink,
        b'3' => TypeFlag::CharacterSpecial,
        b'4' => TypeFlag::BlockSpecial,
        b'5' => TypeFlag::Directory,
        b'6' => TypeFlag::Fifo,
        b'7' => TypeFlag::ContiguousFile,
        b'g' => TypeFlag::PaxGlobal,
        b'x' | b'X' => TypeFlag::Pax,
        b'D' => TypeFlag::GnuDirectory,
        b'K' => TypeFlag::GnuLongLink,
        b'L' => TypeFlag::GnuLongName,
        b'S' => TypeFlag::GnuSparse,
        b'V' => TypeFlag::GnuVolumeHeader,
        b'A'..=b'Z' => TypeFlag::VendorSpecific,
        _ => return Err(nom::Err::Error(error_position!(i, ErrorKind::Fail))),
    };
    Ok((rest, flag))
}

/// [`Sparse`] parsing
fn parse_sparse(i: &[u8]) -> IResult<&[u8], Sparse> {
    let (i, (offset, numbytes)) = pair(parse_octal12, parse_octal12)(i)?;
    Ok((i, Sparse { offset, numbytes }))
}

fn parse_sparses(i: &[u8], count: usize) -> IResult<&[u8], Vec<Sparse>> {
    let mut it = iterator(i, parse_sparse);
    let res = it
        .take(count)
        .filter(|s| !(s.offset == 0 && s.numbytes == 0))
        .collect();
    let (i, ()) = it.finish()?;
    Ok((i, res))
}

fn add_to_vec(sparses: &mut Vec<Sparse>, extra: Vec<Sparse>) -> &mut Vec<Sparse> {
    sparses.extend(extra);
    sparses
}

fn parse_extra_sparses<'a, 'b>(
    i: &'a [u8],
    isextended: bool,
    sparses: &'b mut Vec<Sparse>,
) -> IResult<&'a [u8], &'b mut Vec<Sparse>> {
    if isextended {
        let (i, sps) = parse_sparses(i, 21)?;
        let (i, extended) = parse_bool(i)?;
        let (i, _) = take(7usize)(i)?; // padding to 512

        parse_extra_sparses(i, extended, add_to_vec(sparses, sps))
    } else {
        Ok((i, sparses))
    }
}

/// POSIX ustar extra header
fn parse_extra_posix(i: &[u8]) -> IResult<&[u8], UStarExtraHeader<'_>> {
    let (i, prefix) = terminated(parse_str155, take(12usize))(i)?;
    let header = UStarExtraHeader::Posix(PosixExtraHeader { prefix });
    Ok((i, header))
}

/// GNU ustar extra header
fn parse_extra_gnu(i: &[u8]) -> IResult<&[u8], UStarExtraHeader<'_>> {
    let mut sparses = Vec::new();

    let (i, atime) = parse_octal12(i)?;
    let (i, ctime) = parse_octal12(i)?;
    let (i, offset) = parse_octal12(i)?;
    let (i, _) = take(4usize)(i)?; // longnames
    let (i, _) = take(1usize)(i)?;
    let (i, sps) = parse_sparses(i, 4)?;
    let (i, isextended) = parse_bool(i)?;
    let (i, realsize) = parse_octal12(i)?;
    let (i, _) = take(17usize)(i)?; // padding to 512

    let (i, _) = parse_extra_sparses(i, isextended, add_to_vec(&mut sparses, sps))?;

    let header = GnuExtraHeader {
        atime,
        ctime,
        offset,
        sparses,
        realsize,
    };
    let header = UStarExtraHeader::Gnu(header);
    Ok((i, header))
}

/// Ustar general parser
fn parse_ustar(
    magic: &'static str,
    version: &'static str,
    mut extra: impl FnMut(&[u8]) -> IResult<&[u8], UStarExtraHeader>,
) -> impl FnMut(&[u8]) -> IResult<&[u8], ExtraHeader> {
    move |input| {
        let (i, _) = tag(magic)(input)?;
        let (i, _) = tag(version)(i)?;
        let (i, uname) = parse_str32(i)?;
        let (i, gname) = parse_str32(i)?;
        let (i, devmajor) = parse_octal8(i)?;
        let (i, devminor) = parse_octal8(i)?;
        let (i, extra) = extra(i)?;

        let header = ExtraHeader::UStar(UStarHeader {
            uname,
            gname,
            devmajor,
            devminor,
            extra,
        });
        Ok((i, header))
    }
}

/// Old header padding
fn parse_old(i: &[u8]) -> IResult<&[u8], ExtraHeader<'_>> {
    map(take(255usize), |_| ExtraHeader::Padding)(i) // padding to 512
}

fn parse_header(i: &[u8]) -> IResult<&[u8], PosixHeader<'_>> {
    let (i, name) = parse_str100(i)?;
    let (i, mode) = parse_octal8(i)?;
    let (i, uid) = parse_octal8(i)?;
    let (i, gid) = parse_octal8(i)?;
    let (i, size) = parse_octal12(i)?;
    let (i, mtime) = parse_octal12(i)?;
    let (i, chksum) = parse_str8(i)?;
    let (i, typeflag) = parse_type_flag(i)?;
    let (i, linkname) = parse_str100(i)?;

    let (i, ustar) = alt((
        parse_ustar("ustar ", " \0", parse_extra_gnu),
        parse_ustar("ustar\0", "00", parse_extra_posix),
        parse_old,
    ))(i)?;

    let header = PosixHeader {
        name,
        mode,
        uid,
        gid,
        size,
        mtime,
        chksum,
        typeflag,
        linkname,
        ustar,
    };
    Ok((i, header))
}

fn parse_contents(i: &[u8], size: u64) -> IResult<&[u8], &[u8]> {
    let trailing = size % 512;
    let padding = match trailing {
        0 => 0,
        t => 512 - t,
    };
    terminated(take(size), take(padding))(i)
}

fn parse_entry(i: &[u8]) -> IResult<&[u8], TarEntry<'_>> {
    let (i, header) = parse_header(i)?;
    let (i, contents) = parse_contents(i, header.size)?;
    Ok((i, TarEntry { header, contents }))
}

pub fn parse_tar(i: &[u8]) -> IResult<&[u8], Vec<TarEntry<'_>>> {
    all_consuming(many0(parse_entry))(i)
}

pub fn parse_long_name(i: &[u8]) -> IResult<&[u8], &str> {
    parse_str(i.len())(i)
}

fn parse_pax_item(i: &[u8]) -> IResult<&[u8], (&str, &str)> {
    let (i, len) = map_res(terminated(digit1, tag(" ")), std::str::from_utf8)(i)?;
    let (i, key) = map_res(terminated(take_until("="), tag("=")), std::str::from_utf8)(i)?;
    let (i, value) = map_res(terminated(take_until("\n"), tag("\n")), std::str::from_utf8)(i)?;
    if let Ok(len_usize) = len.parse::<usize>() {
        debug_assert_eq!(len_usize, len.len() + key.len() + value.len() + 3);
    }
    Ok((i, (key, value)))
}

pub fn parse_pax(i: &[u8]) -> IResult<&[u8], HashMap<&str, &str>> {
    let mut it = iterator(i, parse_pax_item);
    let map = it.collect();
    let (i, ()) = it.finish()?;
    Ok((i, map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nom::error::ErrorKind;

    const EMPTY: &[u8] = b"";

    #[test]
    fn parse_octal_ok_test() {
        assert_eq!(parse_octal(b"756", 3), Ok((EMPTY, 494)));
        assert_eq!(parse_octal(b"756\0 234", 8), Ok((EMPTY, 494)));
        assert_eq!(parse_octal(b"756    \0", 8), Ok((EMPTY, 494)));
        assert_eq!(parse_octal(b"", 0), Ok((EMPTY, 0)));
    }

    #[test]
    fn parse_octal_error_test() {
        let t1: &[u8] = b"1238";
        let _e: &[u8] = b"8";
        let t2: &[u8] = b"a";
        let t3: &[u8] = b"A";

        assert_eq!(
            parse_octal(t1, 4),
            Err(nom::Err::Error(error_position!(_e, ErrorKind::OctDigit)))
        );
        assert_eq!(
            parse_octal(t2, 1),
            Err(nom::Err::Error(error_position!(t2, ErrorKind::OctDigit)))
        );
        assert_eq!(
            parse_octal(t3, 1),
            Err(nom::Err::Error(error_position!(t3, ErrorKind::OctDigit)))
        );
    }

    #[test]
    fn parse_str_test() {
        let s: &[u8] = b"foobar\0\0\0\0baz";
        let baz: &[u8] = b"baz";
        assert_eq!(parse_str(10)(s), Ok((baz, "foobar")));
    }

    #[test]
    fn parse_sparses_test() {
        let sparses = std::iter::repeat(0u8).take(12 * 2 * 4).collect::<Vec<_>>();
        assert_eq!(parse_sparses(&sparses, 4), Ok((EMPTY, vec![])));
    }

    #[test]
    fn parse_pax_test() {
        let item: &[u8] = b"25 ctime=1084839148.1212\nfoo";
        let foo: &[u8] = b"foo";
        assert_eq!(
            parse_pax_item(item),
            Ok((foo, ("ctime", "1084839148.1212")))
        );
    }
}
