//! Development tasks. Not shipped, and not a dependency of the application.
//!
//! ```sh
//! cargo run -p xtask -- icons         # regenerate assets/generated/ from assets/icon.png
//! cargo run -p xtask -- bundle-macos  # wrap target/release/hidegit into hideGit.app
//! ```
//!
//! The generated files are committed, because `crates/hidegit/build.rs` needs
//! the `.ico` at build time and contributors should not have to run a task to
//! get a compiling checkout.

use std::{
    error::Error,
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
    process::ExitCode,
};

use icns::{IconFamily, IconType, PixelFormat};
use image::{RgbaImage, imageops::FilterType};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const USAGE: &str = "\
usage: cargo run -p xtask -- <task>

tasks:
  icons          regenerate assets/generated/ from assets/icon.png
  bundle-macos   assemble target/release/bundle/hideGit.app";

/// Sizes packed into the Windows `.ico`.
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// Sizes installed into the Linux hicolor icon theme.
const HICOLOR_SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];

/// The macOS icon grid — exactly the set `iconutil` produces from an
/// `.iconset` directory. The duplicated pixel sizes are intentional: a 32px
/// image is both "16pt at 2x" and "32pt at 1x", and macOS picks between them.
const ICNS_TYPES: [IconType; 10] = [
    IconType::RGBA32_16x16,
    IconType::RGBA32_16x16_2x,
    IconType::RGBA32_32x32,
    IconType::RGBA32_32x32_2x,
    IconType::RGBA32_128x128,
    IconType::RGBA32_128x128_2x,
    IconType::RGBA32_256x256,
    IconType::RGBA32_256x256_2x,
    IconType::RGBA32_512x512,
    IconType::RGBA32_512x512_2x,
];

/// Apple insets an app icon inside its canvas so that everything in the Dock
/// reads at the same optical size: an 824px tile on a 1024px canvas. Our
/// source is a full-bleed tile, so it is scaled into that safe area for macOS
/// and left edge-to-edge everywhere else, where the convention is full-bleed.
const MACOS_TILE_RATIO: f32 = 824.0 / 1024.0;

/// The icon embedded in the binary and handed to the window system.
///
/// 256px is the largest size Windows asks a window for; anything larger is
/// bytes in the binary nobody reads.
const WINDOW_ICON_SIZE: u32 = 256;

/// The size the README renders from.
const README_ICON_SIZE: u32 = 512;

fn main() -> ExitCode {
    let root = repo_root();
    let task = std::env::args().nth(1);

    let result = match task.as_deref() {
        Some("icons") => icons(&root),
        Some("bundle-macos") => bundle_macos(&root),
        other => {
            if let Some(name) = other {
                eprintln!("xtask: unknown task `{name}`\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = result {
        eprintln!("xtask: {error}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

/// The workspace root. `xtask/` sits directly inside it.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ always has a parent directory")
        .to_path_buf()
}

fn icons(root: &Path) -> Result<()> {
    let source = root.join("assets/icon.png");
    let generated = root.join("assets/generated");

    let tile = image::open(&source)?.into_rgba8();
    println!(
        "source {} — {}×{}",
        source.display(),
        tile.width(),
        tile.height()
    );

    let square = pad_to_square(&tile);
    if square.width() != tile.width() || square.height() != tile.height() {
        println!(
            "padded to {0}×{0}, centred rather than stretched",
            square.width()
        );
    }
    println!();

    write_png(
        &scaled(&square, WINDOW_ICON_SIZE),
        &generated.join("window-icon-256.png"),
    )?;
    write_png(
        &scaled(&square, README_ICON_SIZE),
        &generated.join("icon-512.png"),
    )?;

    for size in HICOLOR_SIZES {
        write_png(
            &scaled(&square, size),
            &generated.join(format!(
                "hicolor/{size}x{size}/apps/com.youhide.hidegit.png"
            )),
        )?;
    }

    write_ico(&square, &generated.join("hidegit.ico"))?;
    write_icns(&square, &generated.join("hidegit.icns"))?;

    Ok(())
}

/// Pads to a square canvas, centred, rather than stretching to fit.
///
/// The source tile is a couple of percent wider than it is tall. Stretching it
/// square would distort the squircle, which is the one shape a viewer notices
/// immediately.
fn pad_to_square(source: &RgbaImage) -> RgbaImage {
    let side = source.width().max(source.height());
    if side == source.width() && side == source.height() {
        return source.clone();
    }

    let mut canvas = RgbaImage::new(side, side);
    image::imageops::replace(
        &mut canvas,
        source,
        i64::from(side - source.width()) / 2,
        i64::from(side - source.height()) / 2,
    );
    canvas
}

fn scaled(square: &RgbaImage, size: u32) -> RgbaImage {
    image::imageops::resize(square, size, size, FilterType::Lanczos3)
}

/// Scales the full-bleed tile into Apple's safe area on a transparent canvas.
fn macos_framed(square: &RgbaImage, canvas: u32) -> RgbaImage {
    let tile_size = ((canvas as f32) * MACOS_TILE_RATIO).round().max(1.0) as u32;
    let tile = scaled(square, tile_size);

    let mut framed = RgbaImage::new(canvas, canvas);
    let offset = i64::from(canvas - tile_size) / 2;
    image::imageops::replace(&mut framed, &tile, offset, offset);
    framed
}

fn write_png(image: &RgbaImage, path: &Path) -> Result<()> {
    create_parent(path)?;
    image.save(path)?;
    report(path, Some((image.width(), image.height())), None);
    Ok(())
}

fn write_ico(square: &RgbaImage, path: &Path) -> Result<()> {
    create_parent(path)?;

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in ICO_SIZES {
        let image = ico::IconImage::from_rgba_data(size, size, scaled(square, size).into_raw());
        dir.add_entry(ico::IconDirEntry::encode(&image)?);
    }
    dir.write(BufWriter::new(File::create(path)?))?;

    report(
        path,
        None,
        Some(format!("{} entries {ICO_SIZES:?}", dir.entries().len())),
    );
    Ok(())
}

fn write_icns(square: &RgbaImage, path: &Path) -> Result<()> {
    create_parent(path)?;

    let mut family = IconFamily::new();
    for icon_type in ICNS_TYPES {
        let size = icon_type.pixel_width();
        let framed = macos_framed(square, size);
        let image = icns::Image::from_data(PixelFormat::RGBA, size, size, framed.into_raw())?;
        family.add_icon_with_type(&image, icon_type)?;
    }
    family.write(BufWriter::new(File::create(path)?))?;

    report(
        path,
        None,
        Some(format!(
            "{} icons, 16–1024px, inset to the safe area",
            ICNS_TYPES.len()
        )),
    );
    Ok(())
}

fn create_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn report(path: &Path, dimensions: Option<(u32, u32)>, note: Option<String>) {
    let bytes = fs::metadata(path)
        .map(|meta| meta.len())
        .unwrap_or_default();
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();

    match (dimensions, note) {
        (Some((width, height)), _) => println!("  {name:<32} {width}×{height}  ({bytes} bytes)"),
        (None, Some(note)) => println!("  {name:<32} {note}  ({bytes} bytes)"),
        (None, None) => println!("  {name:<32} ({bytes} bytes)"),
    }
}

/// Assembles an unsigned `.app` around an already-built release binary.
///
/// Unsigned and un-notarised on purpose: this exists so the icon shows in the
/// Dock during development. A signed, notarised `.dmg` is M6.
fn bundle_macos(root: &Path) -> Result<()> {
    let binary = root.join("target/release/hidegit");
    if !binary.is_file() {
        return Err(format!(
            "{} does not exist — run `cargo build --release` first",
            binary.display()
        )
        .into());
    }

    let app = root.join("target/release/bundle/hideGit.app");
    if app.exists() {
        fs::remove_dir_all(&app)?;
    }

    let contents = app.join("Contents");
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(contents.join("Resources"))?;

    let template = root.join("packaging/macos/Info.plist");
    let plist = fs::read_to_string(&template)?.replace("$VERSION", env!("CARGO_PKG_VERSION"));
    fs::write(contents.join("Info.plist"), plist)?;

    // fs::copy carries the executable bit across on Unix.
    fs::copy(&binary, contents.join("MacOS/hidegit"))?;
    fs::copy(
        root.join("assets/generated/hidegit.icns"),
        contents.join("Resources/hidegit.icns"),
    )?;

    println!("{}", app.display());
    println!(
        "\nUnsigned. macOS caches icons aggressively — if the Dock shows a stale\n\
         one, `touch` the bundle or move it."
    );
    Ok(())
}
