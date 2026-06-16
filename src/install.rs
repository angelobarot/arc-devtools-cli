use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

/// The agent skill, embedded at compile time so the binary is self-contained.
const SKILL_MD: &str = include_str!("../skill/arc-devtools/SKILL.md");
const SKILL_NAME: &str = "arc-devtools";

/// Install the agent skill into opencode's skills directory.
///
/// Writes `<skills_dir>/arc-devtools/SKILL.md`. Defaults to the global opencode
/// skills directory (`~/.config/opencode/skills`); override with `dir`.
pub fn install_skill(dir: Option<&str>) -> Result<()> {
    let skills_dir = match dir {
        Some(d) => PathBuf::from(shellexpand_home(d)),
        None => default_skills_dir()?,
    };

    let skill_dir = skills_dir.join(SKILL_NAME);
    let skill_file = skill_dir.join("SKILL.md");

    std::fs::create_dir_all(&skill_dir)
        .with_context(|| format!("creating skill directory {}", skill_dir.display()))?;

    let existed = skill_file.exists();
    std::fs::write(&skill_file, SKILL_MD)
        .with_context(|| format!("writing skill file {}", skill_file.display()))?;

    let verb = if existed { "Updated" } else { "Installed" };
    println!(
        "{verb} the '{SKILL_NAME}' skill at {}",
        skill_file.display()
    );
    println!("Restart opencode to load it.");
    Ok(())
}

/// `~/.config/opencode/skills` — opencode uses `~/.config` on all platforms,
/// including macOS (unlike `dirs::config_dir()`).
fn default_skills_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    Ok(home.join(".config").join("opencode").join("skills"))
}

/// Expand a leading `~` / `~/` to the user's home directory.
fn shellexpand_home(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            let rest = path
                .strip_prefix("~/")
                .or_else(|| path.strip_prefix('~'))
                .unwrap_or("");
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}
