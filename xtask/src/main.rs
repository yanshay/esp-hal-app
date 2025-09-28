use cargo_util_schemas::manifest::{InheritableSemverVersion, TomlManifest, TomlPackage};
#[allow(unused_imports)]
use clap::{builder::PathBufValueParser as _, Args, Parser, Subcommand};
use crc32fast::Hasher;
use serde::Serialize;
use std::{
    fs::{self, File},
    io::{self, BufReader, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(name = "cli")]
#[command(author = "SpoolEase")]
#[command(version = "1.0")]
#[command(about = "A CLI for common esp-hal-app related operations", long_about = None)]
struct Cli {
    #[command(subcommand)]
    main_command: MainCommand,
}

#[derive(Subcommand)]
enum MainCommand {
    /// OTA update commands
    Ota(OtaAndFlasherCommand),
    /// Web Install commands
    WebInstall(OtaAndFlasherCommand), 
    /// License commands
    #[command(subcommand)]
    License(LicenseCommand)
}

// order matters
#[derive(Args)]
struct OtaAndFlasherCommand {
    /// Build the binaries and metadata files
    #[arg(value_enum)]
    build: Option<Build>,

    /// Deploy binaries and metadata files (requires build outputs) - not yet implemented !
    #[arg(value_enum, requires = "build")]
    deploy: Option<Deploy>,

    /// device project folder
    #[arg(long, short)]
    input: PathBuf,

    /// Folder to save artifacts (must exist), if not specified predefined locations are used.
    #[arg(long, short)]
    output: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum LicenseCommand {
    /// Generate private and public keys
    #[command(arg_required_else_help = true)]
    GenKeys {
        /// base file name to store keys
        file: PathBuf,
    },
    /// Generate license binary
    GenBin {
        #[arg(long, short)]
        mac: String,

        /// file to store bin of license
        bin_file: PathBuf,

        key_file: PathBuf,

        #[arg(long, short='g')]
        magic: Option<String>,

        #[arg(long, short)]
        version: semver::Version,

    },
    /// Parse license binary
    ParseBin {
        /// license file to read
        bin_file: PathBuf,

        /// public key to use for parsing
        key_file: PathBuf,

        #[arg(long, short='g')]
        magic: Option<String>,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum Build {
    Build,
}

#[derive(Clone, clap::ValueEnum)]
enum Deploy {
    Deploy,
}

fn main() {
    let cli = Cli::parse();
    
    match cli.main_command {
        MainCommand::Ota(command) => {
            if let Err(e) = handle_ota(&command) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        MainCommand::WebInstall(command) => {
            if let Err(e) = handle_web_install(&command) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        MainCommand::License(license_command) => {
            if let Err(e) = handle_license(&license_command) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
    }
}


// WEB Install and OTA ////////////////////////////////////////////////////////////////////////////////

#[derive(Serialize)]
struct OtaToml {
    filename: String,
    version: String,
    filesize: u64,
    crc32: String,
}

const MANIFEST_TEMPLATE_NEW: &str = r#"{
  "name": "{package_name}",
  "version": "{version}",
  "improv": true,
  "new_install_prompt_erase": false,
  "new_install_improv_wait_time": 30,
  "builds": [
    {
      "chipFamily": "ESP32-S3",
      "parts": [
        { "path": "boot-loader.bin", "offset": 0 },
        { "path": "partition-table.bin", "offset": 32768 },
        { "path": "{bin_name}", "offset": 2097152 }
      ]
    }
  ]
}
"#;

const MANIFEST_TEMPLATE_UPGRADE: &str = r#"{
  "name": "{package_name}",
  "version": "{version}",
  "improv": false,
  "new_install_prompt_erase": true,
  "new_install_improv_wait_time": 30,
  "builds": [
    {
      "chipFamily": "ESP32-S3",
      "parts": [
        { "path": "clear-ota.bin", "offset": 36864 },
        { "path": "{bin_name}", "offset": 2097152 }
      ]
    }
  ]
}
"#;

fn handle_web_install(command: &OtaAndFlasherCommand) -> Result<(), String> {
    if command.build.is_none() && command.deploy.is_none() {
        return Err("At least one command (build or deploy) must be specified".to_string());
    }

    let package_folder_path = command.input.canonicalize().map_err(|e| format!("Error in input path {e}"))?;
    let (package_name, version) = get_package_info(&package_folder_path)?;

    if let Some(Build::Build) = command.build {
        let web_install_folder_path = {
            let result;
            if let Some(output) = &command.output {
                result = output.canonicalize().map_err(|e| format!("Error with output folder (must exist) {e}"))?;
            }
            else {
                let web_install_folder_path = package_folder_path.join("target").join("ota");
                // Create Ota Folder if doen't exist
                std::fs::create_dir_all(&web_install_folder_path).map_err(|e| {
                    format!(
                        "Failed to create folder {} : {e:?}",
                        web_install_folder_path.display()
                    )
                })?;
                result = web_install_folder_path
            }
            result
        };

        let web_install_folder_path = web_install_folder_path.canonicalize().unwrap();

        let bin_name = format!("{package_name}-{version}.bin");

        let (_bin_size, _crc32) = espflash_gen_bin(&package_folder_path, &package_name, &web_install_folder_path, &bin_name)?;

        let manifest_new = MANIFEST_TEMPLATE_NEW.replace("{package_name}", &package_name).replace("{version}", &version.to_string()).replace("{bin_name}", &bin_name);

        let web_install_manifest_new_path = web_install_folder_path.join(format!("manifest-new-{}.json", &version.to_string()));
        std::fs::write(&web_install_manifest_new_path, manifest_new)
            .map_err(|e| format!("Failed writing {} : {e:?}", web_install_manifest_new_path.display()))?;
        println!("Saved new manifest file to {}", web_install_manifest_new_path.display());

        let manifest_upgrade = MANIFEST_TEMPLATE_UPGRADE.replace("{package_name}", &package_name).replace("{version}", &version.to_string()).replace("{bin_name}", &bin_name);
        let web_install_manifest_upgrade_path = web_install_folder_path.join(format!("manifest-upgrade-{}.json", &version.to_string()));
        std::fs::write(&web_install_manifest_upgrade_path, manifest_upgrade)
            .map_err(|e| format!("Failed writing {} : {e:?}", web_install_manifest_upgrade_path.display()))?;
        println!("Saved upgrade manifest file to {}", web_install_manifest_upgrade_path.display());
    }
    Ok(())
}

fn handle_ota(command: &OtaAndFlasherCommand) -> Result<(), String> {
    if command.build.is_none() && command.deploy.is_none() {
        return Err("At least one command (build or deploy) must be specified".to_string());
    }

    let package_folder_path = command.input.canonicalize().map_err(|e| format!("Error in input path '{}' {e}", command.input.display()))?;
    let (package_name, version) = get_package_info(&package_folder_path)?;

    if let Some(Build::Build) = command.build {
        let ota_folder_path = {
            let result;
            if let Some(output) = &command.output {
                result = output.canonicalize().map_err(|e| format!("Error with output folder (must exist) {e}"))?;
            }
            else {
                let ota_folder_path = package_folder_path.join("target").join("ota");
                // Create Ota Folder if doen't exist
                std::fs::create_dir_all(&ota_folder_path).map_err(|e| {
                    format!(
                        "Failed to create folder {} : {e:?}",
                        ota_folder_path.display()
                    )
                })?;
                result = ota_folder_path
            }
            result
        };

        let ota_folder_path = ota_folder_path.canonicalize().unwrap(); // it was either created or canoniclize already so should work

        // let espflash_relative_ota_folder_path = Path::new(".").join("target").join("ota"); // espflash runs with current foder as device package
        let bin_name = format!("{package_name}-{version}.bin");

        let (bin_size, crc32) = espflash_gen_bin(&package_folder_path, &package_name, &ota_folder_path, &bin_name)?;

        // Create toml
        let ota_toml = OtaToml {
            filename: bin_name,
            version: version.to_string(),
            filesize: bin_size,
            crc32: format!("{crc32:x}"),
        };

        let ota_toml_path = ota_folder_path.join("ota.toml");
        let ota_toml = toml::to_string(&ota_toml).expect("Unexpected: failed to serialize toml");
        std::fs::write(&ota_toml_path, ota_toml)
            .map_err(|e| format!("Failed writing {} : {e:?}", ota_toml_path.display()))?;
        println!("Saved metadata information to {}", ota_toml_path.display());
    }

    if let Some(Deploy::Deploy) = command.deploy {
        // TODO: Implement deploy logic
        println!("Deploying OTA update...");
    }

    Ok(())
}

fn espflash_gen_bin(package_folder_path: &std::path::PathBuf, package_name: &str, espflash_relative_ota_folder_path: &std::path::PathBuf, bin_name: &str) -> Result<(u64, u32), String> {
    let espflash_relative_source_bin_folder_path = Path::new(".")
        .join("target")
        .join("xtensa-esp32s3-none-elf")
        .join("release");
    let espflash_relative_source_bin_file_path =
        espflash_relative_source_bin_folder_path.join(&package_name);
    let esp_flash_relative_target_bin_file_path =
        espflash_relative_ota_folder_path.join(bin_name);
    let espflash_cmdline = format!("save-image --partition-table ./partitions.csv --flash-mode dio --flash-freq 80mhz --flash-size 16mb --chip esp32s3 {} {}", espflash_relative_source_bin_file_path.display(), esp_flash_relative_target_bin_file_path.display());
    println!("Executing: espflash {espflash_cmdline}");
    let args: Vec<&str> = espflash_cmdline.split(" ").collect();
    let status = std::process::Command::new("espflash")
        .args(&args)
        .current_dir(&package_folder_path)
        .status()
        .map_err(|e| format!("Failed to execute espflash : {e}"))?;
    if !status.success() {
        return Err("espflash run failed".to_string());
    }
    let espflash_target_bin_file_path = package_folder_path.join(esp_flash_relative_target_bin_file_path);
    println!(
        "Saved firmware binary to {}",
        espflash_target_bin_file_path.display()
    );
    let target_bin_meta = std::fs::metadata(&espflash_target_bin_file_path).map_err(|e| {
        format!(
            "Failed accessing '{}' : {e:?}",
            espflash_target_bin_file_path.display()
        )
    })?;
    let bin_size = target_bin_meta.size();
    let crc32 = compute_crc32(espflash_target_bin_file_path.as_path())
        .map_err(|e| format!("Failed to calculate crc32: {e:?}"))?;
    Ok((bin_size, crc32))
}

fn get_package_info(
    package_folder_path: &std::path::PathBuf,
) -> Result<(String, semver::Version), String> {
    let toml_path = package_folder_path.join("Cargo.toml");
    let cargo_toml = fs::read_to_string(&toml_path)
        .map_err(|e| format!("Can't read '{}' : {e:?}", &toml_path.display()))?;
    let cargo_toml: TomlManifest = toml::from_str(&cargo_toml)
        .map_err(|e| format!("Can't parse '{}' : {e:?}", &toml_path.display()))?;
    let package_name = if let Some(TomlPackage { name, .. }) = cargo_toml.package.as_deref() {
        name.as_ref()
    } else {
        return Err("Package name not fount in Cargo.toml".to_string());
    };
    let version = if let Some(TomlPackage {
        version: Some(InheritableSemverVersion::Value(version)),
        ..
    }) = cargo_toml.package.as_deref()
    {
        version
    } else {
        return Err("Package version not fount in Cargo.toml".to_string());
    };
    Ok((package_name.to_string(), version.clone()))
}

fn compute_crc32(path: &Path) -> Result<u32, io::Error> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Hasher::new();
    let mut buffer = [0u8; 4096]; // 4 KB buffer

    while let Ok(n) = reader.read(&mut buffer) {
        if n == 0 {
            break; // EOF
        }
        hasher.update(&buffer[..n]);
    }

    Ok(hasher.finalize())
}

// WEB Install and OTA ////////////////////////////////////////////////////////////////////////////////

fn handle_license(command: &LicenseCommand) -> Result<(), String> {
    match command {
        LicenseCommand::GenKeys { file } => handle_license_genkeys(file),
        LicenseCommand::GenBin { version, mac, magic, bin_file, key_file } => handle_gen_bin(version, mac, magic, bin_file, key_file),
        LicenseCommand::ParseBin { magic, bin_file, key_file } => handle_parse_bin(magic, bin_file, key_file),
    }
}

fn handle_parse_bin(_magic: &Option<String>, _bin_file: &PathBuf, _key_file: &PathBuf) -> Result<(), String> {
    todo!()
}

fn handle_gen_bin(_version: &semver::Version, _mac: &str, _magic: &Option<String>, _bin_file: &PathBuf, _key_file: &PathBuf) -> Result<(), String> {
    todo!()
}

fn handle_license_genkeys(_file: &PathBuf) -> Result<(), String> {
    todo!()
}


