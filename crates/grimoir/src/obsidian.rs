use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use grimoir_library::Annotation;
use grimoir_pdf::RawPage;

fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            slug.push(ch);
        } else if (ch.is_whitespace() || ch == '-' || ch == '_') && !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug = slug.trim_matches('_');
    if slug.is_empty() {
        "book".to_owned()
    } else {
        slug.chars().take(80).collect()
    }
}

fn yaml_escape(value: &str) -> String {
    value.replace('"', "'").replace('\n', " ")
}

/// Export one rendered page (and its annotations) to an Obsidian vault.
/// The note is created on first use and appended to afterwards.
pub fn export_page(
    vault: &Path,
    book_title: &str,
    page: usize,
    raw: &RawPage,
    annotations: &[Annotation],
) -> Result<PathBuf> {
    let slug = slugify(book_title);
    let notes_dir = vault.join("Grimoir");
    let attachments = notes_dir.join("attachments");
    fs::create_dir_all(&attachments)
        .with_context(|| format!("creating {}", attachments.display()))?;

    let image_name = format!("{slug}-p{:04}.png", page + 1);
    let image_path = attachments.join(&image_name);
    image::save_buffer_with_format(
        &image_path,
        &raw.rgba,
        raw.width,
        raw.height,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .with_context(|| format!("writing {}", image_path.display()))?;

    let note_path = notes_dir.join(format!("{slug}.md"));
    let is_new = !note_path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&note_path)
        .with_context(|| format!("opening {}", note_path.display()))?;

    if is_new {
        writeln!(file, "---")?;
        writeln!(file, "title: \"{}\"", yaml_escape(book_title))?;
        writeln!(file, "source: Grimoir")?;
        writeln!(file, "---\n")?;
        writeln!(file, "# {book_title}\n")?;
    }

    let relative_image = format!("Grimoir/attachments/{image_name}");
    writeln!(file, "## Page {}\n", page + 1)?;
    writeln!(file, "![[{relative_image}]]\n")?;

    let page_annotations = annotations
        .iter()
        .filter(|annotation| annotation.page == page)
        .collect::<Vec<_>>();
    if !page_annotations.is_empty() {
        writeln!(file, "### Highlights\n")?;
        for (index, annotation) in page_annotations.iter().enumerate() {
            if let Some(note) = annotation.note.as_deref().filter(|note| !note.is_empty()) {
                writeln!(file, "> [!note] Highlight {}\n> {}\n", index + 1, note)?;
            } else {
                writeln!(file, "> [!note] Highlight {}\n", index + 1)?;
            }
            if let Some(rects) = annotation.rects_json.as_deref() {
                writeln!(file, "> Region: `{rects}`\n")?;
            }
        }
    }

    Ok(note_path)
}

/// Read the configured vault path. Environment overrides the database so
/// development and portable setups do not need to mutate the profile.
pub fn vault_from_env() -> Option<PathBuf> {
    std::env::var_os("GRIMOIR_OBSIDIAN_VAULT").map(PathBuf::from)
}
