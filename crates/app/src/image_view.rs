//! Inline slideshow image rendering.
//!
//! Without the `slideshow-image` cargo feature this module only exposes a
//! [`SUPPORTED`] flag and a stub [`view`] that returns `Err`. With the feature
//! enabled it temporarily suspends the ratatui alternate screen, prints the
//! image with viuer (ANSI half-blocks by default, sixel / kitty / iterm2 when
//! the terminal advertises support), waits for any keystroke, and restores
//! the alternate screen. The caller is responsible for forcing a redraw on
//! return.

use std::io;

/// `true` when the binary was built with `--features slideshow-image`.
#[cfg(feature = "slideshow-image")]
pub const SUPPORTED: bool = true;

/// `true` when the binary was built with `--features slideshow-image`.
#[cfg(not(feature = "slideshow-image"))]
pub const SUPPORTED: bool = false;

/// Render `bytes` as an image inline. `width_cells` is a hint for the maximum
/// width in terminal cells; viuer scales preserving aspect ratio.
#[cfg(feature = "slideshow-image")]
pub fn view(bytes: &[u8], width_cells: u32) -> io::Result<()> {
    use crossterm::{
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };

    let img = image::load_from_memory(bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Drop out of the alternate screen so the image lands in the user's
    // normal scrollback. crossterm's raw mode also has to be off for the
    // line-buffered prompt read to behave normally.
    disable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, LeaveAlternateScreen)?;

    let result = (|| -> io::Result<()> {
        let config = viuer::Config {
            width: Some(width_cells.max(20)),
            absolute_offset: false,
            transparent: true,
            ..Default::default()
        };
        viuer::print(&img, &config).map_err(io::Error::other)?;
        println!();
        println!("[press Enter to return to dab-rtl]");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
        Ok(())
    })();

    // Always restore the alt screen + raw mode, even if rendering failed.
    let _ = execute!(io::stdout(), EnterAlternateScreen);
    let _ = enable_raw_mode();
    result
}

/// Stub used when the feature is disabled.
#[cfg(not(feature = "slideshow-image"))]
pub fn view(_bytes: &[u8], _width_cells: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "slideshow-image feature not enabled (rebuild with --features slideshow-image)",
    ))
}
