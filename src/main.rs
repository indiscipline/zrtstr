// TODO:
// * IEEE Float writing support
// * Tests

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
        }
        None => {
            // If no file name present, process all WAVs in current dir
            println!("No input file given. Processing current directory.");
            let current_dir = env::current_dir().unwrap();
            for dir_entry in read_dir(current_dir).expect("Can't read current directory") {
                if let Ok(entry) = dir_entry {
                    // scan each directory entry, if accessible
                    // get path String
                    let path_str = entry.path().to_str().unwrap_or("").to_string();
                    let res = if validate_path(path_str.clone()).is_ok() {
                        // if file has wav extension
                        process_file(&path_str, &conf)
                    } else {
                        // ignore other files
                        Ok(())
                    };
                    if let Err(err) = res {
                        println!("{}", err.to_string());
                    };
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

    if zero_test(reader, conf.dither, spec.sample_format) {
        println!("\nFile is not double mono, channels are different!");
    } else {
        println!("\nChannels are identical! Faux stereo detected");
        if !conf.dry_run {
            try!(copy_to_mono(fname, &spec, conf.no_overwrites));
        }
    }
    Ok(())
}

/// Check if data in each pair of samples is identical, or lies within given difference
fn zero_test<R: std::io::Read>(mut reader: WavReader<R>,
                               dither_threshold: u32,
                               format: SampleFormat)
                               -> bool {

    let dur_samples = reader.duration();
    let progress_chunk = dur_samples / 100;

    // println!("Duration in samples={},  Sample rate={}",reader.duration(),spec.sample_rate);

    // Initialize progress bar
    let mut pb = ProgressBar::new(dur_samples as u64);

    // Closure for updating progress bar
    let mut update_pb = |sample_num: usize| {
        if sample_num as u32 % progress_chunk == 0 {
            pb.add(progress_chunk as u64);
        } else if sample_num as u32 == dur_samples - 1 {
            // - 1 since comparing the left channel
            pb.finish();
        }
    };

    match format {
        // TODO: (?) Write a macro to unify logic for both formats
        //
        // Process INT samples
        SampleFormat::Int => {
            // Define a closure which compares the difference of two samples.
            // If dither_threshold is given, compare to it, else it must be 0
            let comparator: Box<Fn(i32) -> bool> = if dither_threshold == 0 {
                Box::new(|x: i32| x != 0)
            } else {
                Box::new(|x: i32| x.abs() as u32 > dither_threshold)
            };

            reader.samples::<i32>()
                .enumerate()
                .batching(|mut it| {
                    match it.next() {
                        None => None,
                        Some(x) => {
                            match it.next() {
                                None => None,
                                Some(y) => {
                                    update_pb(x.0);
                                    Some(x.1.unwrap() - y.1.unwrap())
                                }
                            }
                        }
                    }
                })
                .any(|diff| comparator(diff)) //Actual comparison via closure
        }
        // Process FLOAT samples
        SampleFormat::Float => {
            // Define a closure which compares the difference of two samples.
            // If dither_threshold is given, compare to it, else it must be 0
            let comparator: Box<Fn(f32) -> bool> = if dither_threshold == 0 {
                Box::new(|x: f32| x != 0f32)
            } else {
                // Average 16-bit dither sample is ~ 0.000117
                // However, fluctuations are quite high, short tests showed
                // x10 multiplier (=default one) for the Threshold to be reasonable.
                // !! Needs more research!
                Box::new(|x: f32| x.abs() > dither_threshold as f32 * 0.000117f32)
            };
            reader.samples::<f32>()
                .enumerate()
                .batching(|mut it| {
                    match it.next() {
                        None => None,
                        Some(x) => {
                            match it.next() {
                                None => None,
                                Some(y) => {
                                    update_pb(x.0);
                                    Some(x.1.unwrap() - y.1.unwrap())
                                }
                            }
                        }
                    }
                })
                .any(|diff| comparator(diff)) //Actual comparison via closure
        }
    }
}

/// Copy left channel of the input file to mono wav
fn copy_to_mono(input_fname: &str, spec: &WavSpec, no_overwrites: bool) -> Result<(), String> {
    println!("  * Converting to true-mono...");

    // TODO: Remove as Hound supports writing float data
    if spec.sample_format == SampleFormat::Float {
        return Err("  ! Writing IEEE_FLOAT files not implemented yet!".to_string());
    }

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
                SampleFormat::Float => unreachable!(),
                // stream_samples!(f32, reader, writer, error_occurred),
                SampleFormat::Int => stream_samples!(i32, reader, writer, error_occurred),
            }
        }
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
