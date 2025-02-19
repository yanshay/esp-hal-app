use std::{borrow::Cow, error::Error, io::{self}};
use anyhow::anyhow;

use clap::Parser;
use espflash::{
    cli::{config::Config, *},
    elf::RomSegment,
};
use miette::Result;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Parser)]
#[command(about, max_term_width = 100, propagate_version = true, version, arg_required_else_help = true)]
pub struct MyCli {
    /// url for (esp-web-tools) manifest file
    url: String,

    /// Don't erase device before flashing (default false, so erase)
    #[arg(long, required = false, default_value="false")]
    dont_erase: bool,

    /// Connection configuration
    #[clap(flatten)]
    pub connect_args: ConnectArgs,
}

// const MANIFEST_TEMPLATE: &str = r#"{
//   "name": "{package_name}",
//   "version": "{version}",
//   "improv": true,
//   "new_install_prompt_erase": false,
//   "new_install_improv_wait_time": 30,
//   "builds": [
//     {
//       "chipFamily": "ESP32-S3",
//       "parts": [
//         { "path": "boot-loader.bin", "offset": 0 },
//         { "path": "partition-table.bin", "offset": 32768 },
//         { "path": "{bin_name}", "offset": 2097152 }
//       ]
//     }
//   ]
// }

#[derive(Deserialize, Debug)]
struct ManifestBuildPart {
    path: String,
    offset: u32,
}
#[derive(Deserialize, Debug)]
struct ManfestBuild {
    parts: Vec<ManifestBuildPart>,
}
#[derive(Deserialize, Debug)]
struct Manifest {
    name: String,
    version: String,
    builds: Vec<ManfestBuild>
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = MyCli::parse();

    let mut connect_args = args.connect_args;
    if connect_args.baud.is_none() {
        connect_args.baud = Some(921600);
    }

    let config = Config::load()?;
    println!();
    println!("Loading manifest file {}",args.url);
    let manifest_json = String::from_utf8(download_file(&args.url)?)?;
    let manifest = serde_json::from_str::<Manifest>(&manifest_json)?;
    println!("Found manifest for {} version {}", manifest.name, manifest.version);
    let manifest_url = Url::parse(&args.url)?;
    let parts_base_url = manifest_url.join("./")?;

    let mut segments = Vec::<RomSegment>::new();
    let parts = &manifest.builds.get(0).ok_or(anyhow!("No builds in manifest"))?.parts;
    for part in parts {
        let bin_url = parts_base_url.join(&part.path)?;
        println!(" - Loading {}", part.path);
        let bin = download_file(&bin_url.to_string())?;
        segments.push(RomSegment {addr: part.offset, data: Cow::Owned(bin)})
    }

    println!(
r#"
--------------------------------------------------------------------------------
Your device will now be {}flashed with {} vesion {}.
Press Ctrl-C Now to cancel installation.

Please connect your device via USB to your computer.
Then press enter/return to continue.
--------------------------------------------------------------------------------"#, 
        if args.dont_erase { "" } else { "erased and then " },
        manifest.name, manifest.version);

    readln();
    let mut flasher = connect(&connect_args, &config, false, false)?;
    print_board_info(&mut flasher)?;
    if !args.dont_erase {
        println!("\nErasing device flash... this may take a couple of minutes with no progress indication");
        flasher.erase_flash().unwrap();
    }

    println!("Erasing done, now flashing\n");
    flasher.write_bins_to_flash(&segments, Some(&mut EspflashProgress::default()))?;

    println!(
r#"

----------------------------------------------------------
Successfully flashed SpoolEase to device.
Follow setup instructions on the device to continue setup.
----------------------------------------------------------"#);

    Ok(())
}

fn readln() -> String {
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string()
}

fn download_file(url: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let client = reqwest::blocking::Client::new();
    let response = client.get(url).send()?;
    
    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()).into());
    }
    
    let bytes = response.bytes()?;
    Ok(bytes.to_vec())
}
