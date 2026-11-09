//! Starts a "Sign in with Plex" request on plex.tv (both the strong PIN for the
//! browser page and the short plex.tv/link code), checks it can be polled, and
//! prints the sign-in page address (CI opens it in a browser for a screenshot).
//!
//!     cargo run -p unitn-core --example plex_pin_check -- [file-for-the-url]

use unitn_core::servers;

fn main() {
    let out = std::env::args().nth(1);
    let result = (|| -> anyhow::Result<String> {
        let pin = servers::plex_pin_start()?;
        println!("strong code: {} characters", pin.code.len());
        println!("link code: {:?}", pin.link_code);
        println!("sign-in page: {}", pin.auth_url.replace(&pin.code, "<code>"));
        anyhow::ensure!(pin.code.len() > 8, "the browser code should be a strong (long) PIN");
        anyhow::ensure!(pin.link_code.as_deref().is_some_and(|c| c.len() <= 8), "missing the short plex.tv/link code");
        anyhow::ensure!(servers::plex_pin_token(&pin)?.is_none(), "a new sign-in can't be approved yet");
        if let Some(path) = &out {
            std::fs::write(path, &pin.auth_url)?;
        }
        Ok(format!("strong code {} chars, link code {}", pin.code.len(), pin.link_code.unwrap_or_default()))
    })();
    match result {
        Ok(info) => println!("RESULT plex sign-in: OK ({info})"),
        Err(e) => {
            println!("RESULT plex sign-in: FAILED: {e:#}");
            std::process::exit(1);
        }
    }
}
