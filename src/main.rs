// TODO:
// * IEEE Float support
// * Tests
// * Refactor copy_to_mono nestiness.

extern crate hound;
extern crate itertools;
extern crate pbr;
#[macro_use]
extern crate clap;

mod config;

use std::path::Path;
use std::fs::{remove_file, read_dir};
use std::env;
use hound::{WavReader, WavSpec, WavWriter, SampleFormat};
use itertools::Itertools;
use pbr::ProgressBar;

use config::{Conf, validate_path};

fn main() {
    let (input_fname, conf) = config::get();

    if conf.dither > 100 {
        println!("  ! Dither threshold probably set too high! False positives possible.")
    }

    match input_fname {
        Some(fname) => {
            match process_file(&fname, &conf) {
                Ok(_) => {}
                Err(err) => println!("{}", err.to_string()),
            }
        },
        None => {
            // If no file name present, process all WAVs in current dir
            println!("No input file given. Processing current directory.");
            let current_dir = env::current_dir().unwrap();
            for dir_entry in read_dir(current_dir).expect("Can't read current directory") {
                match dir_entry.map(|entry| {
                    // scan each directory entry, if accessible
                    let path_str = entry.path().to_str().unwrap_or("").to_string(); //get path String
                    validate_path(path_str.clone()).map(|_| {
                        // if it has wav extension
                        match process_file(&path_str, &conf) {// process path
                            Ok(_) => Ok(()),
                            Err(err) => Err(err.to_string()),
                        }
                    })
                }) {
                    Ok(_) => {}
                    Err(err) => println!("{}", err.to_string()),
                }
            }
        }
    }
}

fn read_file(fname: &str) -> Result<WavReader<std::io::BufReader<std::fs::File>>, String> {
    WavReader::open(fname).map_err(|err| err.to_string())
}

fn process_file(fname: &str, conf: &Conf) -> Result<(), String> {
    println!("--- Analyzing: {}", fname);
    let reader = try!(read_file(fname));
    let spec = reader.spec();

    if spec.channels != 2 {
        return Err(String::from("File is not stereo! Exiting"));
    }
    if spec.sample_format == SampleFormat::Float {
        return Err(String::from("IEEE Float files are not supported! Exiting"));
    }

    if zero_test(reader, conf.dither) {
        println!("\nFile is not double mono, channels are different!");
        Ok(())
    } else {
        println!("\nChannels are identical! Faux stereo detected");
        if !conf.dry_run {
            try!(copy_to_mono(fname, &spec, conf.no_overwrites));
        }
        Ok(())
    }
}

/// Check if data in each pair of samples is identical, or lies within given difference
fn zero_test<R: std::io::Read>(mut reader: WavReader<R>, dither_threshold: u32) -> bool {

    // Define a closure which compares the difference of two samples.
    // If dither_threshold is given, compare to it, else it must be 0
    let comparator: Box<Fn(i32) -> bool> = if dither_threshold == 0 {
        Box::new(|x: i32| x != 0)
    } else {
        Box::new(|x: i32| x.abs() as u32 > dither_threshold)
    };

    let dur_samples = reader.duration();
    let progress_chunk = dur_samples as u64 / 100;
    let progress_iter = (1..progress_chunk + 1).cycle();

    // println!("Duration in samples={},  Sample rate={}",reader.duration(),spec.sample_rate);

    // Initialize progress bar
    let mut pb = ProgressBar::new(dur_samples as u64);

    // Read pairs of samples, update progress bar each progress_chunk iteration.
    reader.samples::<i32>()
        .zip(progress_iter)
        .batching(|mut it| {
            match it.next() {
                None => None,
                Some(x) => {
                    match it.next() {
                        None => None,
                        Some(y) => {
                            if y.1 >= progress_chunk {
                                pb.add(progress_chunk);
                            };
                            Some(x.0.unwrap() - y.0.unwrap())
                        }
                    }
                }
            }
        })
        .any(|diff| comparator(diff)) //Actual comparison via closure
}



/// Copy left channel of the input file to mono wav
fn copy_to_mono(input_fname: &str, spec: &WavSpec, no_overwrites: bool) -> Result<(), String> {
    println!("  * Converting to true-mono...");

    let new_spec = WavSpec {
        channels: 1,
        sample_rate: spec.sample_rate,
        bits_per_sample: spec.bits_per_sample,
        sample_format: spec.sample_format,
    };

    let mut reader = try!(read_file(input_fname));

    let output_path = Path::new(input_fname).with_extension("MONO.wav");
    if output_path.exists() {
        print!("Target file already exists. ");
        if no_overwrites {
            print!("Skipping.\n");
            return Ok(());
        } else {
            print!("Replacing...\n");
        };
    }

    let mut writer = try!(WavWriter::create(&output_path, new_spec).map_err(|err| err.to_string()));
    let mut error_occurred = false;

    // Macros for abstracting sample-copying logic, streaming from reader to writer
    macro_rules! stream_samples {
        ($num:ty, $reader:ident, $writer:ident, $error:ident) => {
            for sample in $reader.samples::<$num>().step(2) {
                if $writer.write_sample(sample.unwrap()).is_err() {
                    $error = true;
                    println!("Failed to write sample");
                    break;
                }
            }
        }
    }

    match spec.bits_per_sample {
        8 => stream_samples!(i8, reader, writer, error_occurred),
        16 => stream_samples!(i16, reader, writer, error_occurred),
        24 | 32 => {
            match spec.sample_format {
                SampleFormat::Float =>
                    stream_samples!(f32, reader, writer, error_occurred),
                SampleFormat::Int =>
                    stream_samples!(i32, reader, writer, error_occurred),
            }
        },
        _ => {
            error_occurred = true;
            println!("Can't write a file! Unsupported sample rate requested!");
        }
    }

    if writer.finalize().is_err() {
        error_occurred = true;
        println!("Failed to finalize wav file");
    }

    // Cleaning up on write errors.
    if error_occurred {
        if remove_file(&output_path).is_err() {
            println!("Error removing created file, clean up manually.");
        }
        Err(format!("Failed writing \"{}\"", output_path.to_str().unwrap()))
    } else {
        println!("\"{}\" successfully written!",
            output_path.to_str().unwrap());
        Ok(())
    }
}

#[cfg(test)]

// TODO write tests
mod tests {
    #[test]
    fn test() {}
}
