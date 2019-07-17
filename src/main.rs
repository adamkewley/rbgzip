extern crate clap;
extern crate memmap;
extern crate flate2;

use std::path::Path;
use std::fs::File;
use std::io;
use std::io::{Result, Error, ErrorKind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc};
use std::thread;
use std::thread::{JoinHandle};
use std::sync::mpsc::{sync_channel, Receiver};

use clap::{Arg, App};
use memmap::{MmapOptions, Mmap};
use flate2::bufread::GzDecoder;


struct BgzfHeader {
    bsize: u16,
}

fn has_bgzf_eof_marker(buf: &[u8]) -> bool {
    const BGZF_EOF: [u8; 28] = [
        0x1f, 0x8b, 0x08, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff,
        0x06, 0x00, 0x42, 0x43, 0x02, 0x00, 0x1b, 0x00, 0x03, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00
    ];
    
    let mut buf_end: [u8; 28] = [0; 28];
    buf_end.copy_from_slice(&buf[buf.len() - 28..]);

    for i in 0..28 {
        if BGZF_EOF[i] != buf_end[i] {
            return false;
        }
    }
    return true;
}

// SAM spec: https://samtools.github.io/hts-specs/SAMv1.pdf
// Specifically, BGZF compresssion format (header), which is a
// specialization of gzip RFC 1952
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

fn decompress_gz(buf: &[u8]) -> Result<Vec<u8>> {
    let mut gz = GzDecoder::new(buf);
    let mut out = vec![];

    io::copy(&mut gz, &mut out)?;
    
    Ok(out)
}

struct BgzfBlockPos {
    offset: usize,
    size: u16,
}

// todo: error results...
struct WorkerOutput {
    idx: usize,
    data: Vec<u8>,
}

struct Worker {
    jh: JoinHandle<()>,
    output: Receiver<WorkerOutput>,
}

fn handle_input(buf: Mmap) -> Result<()> {
    const BGZF_MIN_SZ: usize = 28;
    if buf.len() < BGZF_MIN_SZ {
        return Err(Error::new(ErrorKind::InvalidData, "Input data is too small for a bam file. A bam file is *at least* 28 bytes long (i.e. an EOF marker)"));
    }

    if !has_bgzf_eof_marker(&buf[..]) {
        return Err(Error::new(ErrorKind::InvalidData, "Input missing bgzf EOF marker"));
    }    
    
    let blks = {
        let mut blks = vec![];
        let mut off: usize  = 0;    
        while off < buf.len() {
            let hdr = parse_bgzf_header(&buf[off..])?;
            let bsize = hdr.bsize + 1;
            // todo: assert bsize in buf bounds
            blks.push(BgzfBlockPos {
                offset: off,
                size: bsize,
            });
            off += bsize as usize;
        }
        blks
    };

    // Shared between threads to distribute work
    let num_blks = blks.len();
    let buf = Arc::new(buf);
    let blks = Arc::new(blks);
    let in_idx = Arc::new(AtomicUsize::new(0));

    // These two can heavily affect thread balancing.
    const NUM_WORKERS: usize = 11;
    const BUF_SIZE: usize = 8 * NUM_WORKERS;
    
    let mut workers = vec![];    
    for _ in 0..NUM_WORKERS {
        let buf = Arc::clone(&buf);
        let blks = Arc::clone(&blks);
        let in_idx = Arc::clone(&in_idx);
        let (tx, rx) = sync_channel(BUF_SIZE);

        let jh = thread::spawn(move || {
            loop {
                let v = (*in_idx).fetch_add(1, Ordering::Relaxed);
                if v >= num_blks {
                    break;
                }
                let input = (*blks).get(v).unwrap();
                let block_data = &buf[input.offset..input.offset+(input.size as usize)];
                let data = decompress_gz(block_data).unwrap();
                
                tx.send(WorkerOutput {
                    idx: v,
                    data: data
                }).unwrap();
            }
        });

        workers.push(Worker {
            jh: jh,
            output: rx,
        });
    }

    let mut peeks: Vec<_> = workers
        .iter()
        .map(|worker| worker.output.iter().peekable())
        .collect();

    // Main thread is responsible for emitting the outputs in-order
    let mut cur_idx = 0;
    loop {        
        if cur_idx >= blks.len() {
            break;
        }
        
        for peek in peeks.iter_mut() {
            if let Some(output) = peek.peek() {
                if output.idx == cur_idx {
                    let mut s = &output.data[..];
                    io::copy(&mut s, &mut io::stdout())?;
                    peek.next();  // dequeue it
                    cur_idx += 1;
                }
            }
        }
    }

    for worker in workers {
        worker.jh.join().unwrap();
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
             .help("write on standard output, keep original files unchanged")
             .required(true))
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
