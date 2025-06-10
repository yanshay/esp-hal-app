// Cargo.toml dependencies needed:
// [dependencies]
// flate2 = "1.0"
// 
// [build-dependencies] (if using in build.rs)
// flate2 = "1.0"

// lib.rs or your proc macro crate
use proc_macro::TokenStream;
use quote::quote;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use flate2::{write::GzEncoder, Compression};

/// A procedural macro that includes a file's contents as gzipped bytes at compile time.
/// 
/// This macro works similarly to `include_bytes!` but compresses the file content
/// using gzip before embedding it in the binary.
/// 
/// **Note**: Due to proc macro limitations on stable Rust, paths are resolved relative 
/// to the cargo manifest directory (project root), not the calling file.
/// 
/// # Example
/// 
/// ```rust
/// // Include a compressed text file (relative to project root)
/// const COMPRESSED_DATA: &[u8] = include_bytes_gz!("src/data.txt");
/// 
/// // Include config file
/// const COMPRESSED_CONFIG: &[u8] = include_bytes_gz!("config/settings.json");
/// ```
#[proc_macro]
pub fn include_bytes_gz(input: TokenStream) -> TokenStream {
    let input_str = input.to_string();
    
    // Parse the string literal (remove quotes)
    let file_path = input_str.trim_matches('"');
    
    // On stable Rust, we resolve paths relative to CARGO_MANIFEST_DIR
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| ".".to_string());
    
    // Resolve the path relative to the manifest directory
    let full_path = if Path::new(file_path).is_absolute() {
        PathBuf::from(file_path)
    } else {
        Path::new(&manifest_dir).join(file_path)
    };
    
    // Read the file
    let file_contents = match fs::read(&full_path) {
        Ok(contents) => contents,
        Err(e) => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("Failed to read file '{}': {}", full_path.display(), e)
            ).to_compile_error().into();
        }
    };
    
    // Compress the contents using gzip
    let compressed_data = match compress_data(&file_contents) {
        Ok(data) => data,
        Err(e) => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("Failed to compress file '{}': {}", full_path.display(), e)
            ).to_compile_error().into();
        }
    };
    
    // Generate the byte array literal
    let bytes = compressed_data.iter().copied();
    
    let expanded = quote! {
        &[#(#bytes),*]
    };
    
    TokenStream::from(expanded)
}

fn compress_data(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    encoder.finish()
}
