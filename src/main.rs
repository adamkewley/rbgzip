extern crate clap;
extern crate memmap;
extern crate flate2;

use std::path::Path;
use std::fs::File;
use std::io;
use std::io::{Result, Error, ErrorKind};

use clap::{Arg, App};
use memmap::{MmapOptions, Mmap};
use flate2::bufread::GzDecoder;


struct BgzfHeader {
    bsize: u16,
}

struct BgzfBlockPos {
    offset: usize,
    size: u16,
}

fn parse_bgzf_header(buf: &[u8]) -> Result<BgzfHeader> {
    const MIN_BGZF_HDR_SIZE: usize = 16;
    
    if buf.len() < MIN_BGZF_HDR_SIZE {
        return Err(Error::new(ErrorKind::InvalidData, "input too small"));
    }
    
    if buf[0] != 31 || buf[1] != 139 {
        return Err(Error::new(ErrorKind::InvalidData, "input does not start with gzip magic nums"));
    }

    if buf[2] != 8 {
        return Err(Error::new(ErrorKind::InvalidData, "CM field in gzip header is invalid for a BGZF file"));
    }

    if buf[3] != 4 {
        return Err(Error::new(ErrorKind::InvalidData, "FLGs field in gzip header invalid for a BGZF (BAM) file"));
    }

    let xlen: u16 = (buf[10] as u16) | ((buf[11] as u16) << 8);    

    const REQ_FIELDS_SIZE: usize = 12;
    if (xlen as usize) + REQ_FIELDS_SIZE > buf.len() {
        return Err(Error::new(ErrorKind::InvalidData, "Not enough room left in data to accomodate FEXTRA fields"));
    }

    let mut off = REQ_FIELDS_SIZE as usize;
    let end = off + (xlen as usize);
    while off  < end {
        const FEXTRA_FIELD_MIN_SZ: usize = 4;
        if end - off < FEXTRA_FIELD_MIN_SZ {
            return Err(Error::new(ErrorKind::InvalidData, "Ran out of data when reading FEXTRA field"))
        }

        let si1 = buf[off];
        off += 1;
        let si2 = buf[off];
        off += 1;
        let slen = (buf[off] as u16) | ((buf[off+1] as u16) << 8);
        off += 2;

        if off + (slen as usize) > end {
            return Err(Error::new(ErrorKind::InvalidData, "Ran out of data when reading FEXTRA field: out of bounds slen field"));
        }

        if si1 == 66 && si2 == 67 && slen == 2 {
            // it's the header we want
            let bsize = (buf[off] as u16) | ((buf[off+1] as u16) << 8);
            return Ok(BgzfHeader { bsize: bsize });
        } else {
            off += slen as usize;  // skip
        }
    }
    
    return Err(Error::new(ErrorKind::InvalidData, "BC BGZF header not found in gzip Xtra flags"));
}

fn decompress_gz(buf: &[u8]) {
    let mut gz = GzDecoder::new(buf);
    io::copy(&mut gz, &mut io::stdout());
}

fn handle_input(buf: Mmap) -> Result<()> {
    let mut blk_positions: Vec<BgzfBlockPos> = vec![];
    let mut off: usize  = 0;
    while off < buf.len() {
        let hdr = parse_bgzf_header(&buf[off..])?;
        let bsize = hdr.bsize + 1;
        // todo: assert bsize in buf bounds
        blk_positions.push(BgzfBlockPos {
            offset: off,
            size: bsize,
        });
        off += bsize as usize;
    }

    for pos in blk_positions {
        let start = pos.offset;
        let end = start + (pos.size as usize);
        decompress_gz(&buf[start..end]);
    }

    Ok(())
}

fn main() {
    let args = App::new("bam2sam")
        .version("1.0")
        .arg(Arg::with_name("decompress")
             .short("d")
             .help("decompress input")
             .required(true))
        .arg(Arg::with_name("force")
             .short("f")
             .help("force writing to terminal"))
        .arg(Arg::with_name("stdout")
             .short("c")
             .help("write on standard output, keep original files unchanged"))
        .arg(Arg::with_name("FILE")
             .help("input BAM")
             .required(true))
        .get_matches();

    let pth = Path::new(args.value_of("FILE").unwrap());

    if !pth.exists() {
        println!("{}: no such file or directory", pth.to_str().unwrap());
        std::process::exit(1);
    }
    if !pth.is_file() {
        println!("{}: is not a regular file", pth.to_str().unwrap());
        std::process::exit(1);
    }

    let file = File::open(pth).unwrap();
    let mmap = unsafe {
        MmapOptions::new().map(&file).unwrap()
    };
    
    handle_input(mmap).unwrap();
}
