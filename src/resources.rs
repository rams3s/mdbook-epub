use failure::{self, Error, ResultExt};
use mdbook::book::BookItem;
use mdbook::renderer::RenderContext;
use mime_guess::{self, Mime};
use pulldown_cmark::{Event, Parser, Tag};
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::copy;
use std::path::{Path, PathBuf};
use url::Url;

pub fn find(ctx: &RenderContext) -> Result<Vec<Asset>, Error> {
    let mut assets = Vec::new();
    let src_dir = ctx
        .root
        .join(&ctx.config.book.src)
        .canonicalize()
        .context("Unable to canonicalize the src directory")?;

    for section in ctx.book.iter() {
        if let BookItem::Chapter(ref ch) = *section {
            trace!("Searching {} for links and assets", ch);

            let full_path = src_dir.join(&ch.path);
            let parent = full_path
                .parent()
                .expect("All book chapters have a parent directory");
            let found = assets_in_markdown(&ch.content, parent, &ctx.destination.join("cache"))?;

            for full_filename in found {
                let relative = full_filename.strip_prefix(&src_dir).unwrap();
                assets.push(Asset::new(relative, &full_filename));
            }
        }
    }

    Ok(assets)
}

#[derive(Clone, PartialEq, Debug)]
pub struct Asset {
    /// The asset's absolute location on disk.
    pub location_on_disk: PathBuf,
    /// The asset's filename relative to the `src/` directory.
    pub filename: PathBuf,
    pub mimetype: Mime,
}

impl Asset {
    fn new<P, Q>(filename: P, absolute_location: Q) -> Asset
    where
        P: Into<PathBuf>,
        Q: Into<PathBuf>,
    {
        let location_on_disk = absolute_location.into();
        let mt = mime_guess::guess_mime_type(&location_on_disk);

        Asset {
            location_on_disk,
            filename: filename.into(),
            mimetype: mt,
        }
    }
}

fn assets_in_markdown(
    src: &str,
    parent_dir: &Path,
    cache_dir: &Path,
) -> Result<Vec<PathBuf>, Error> {
    let mut found = Vec::new();

    for event in Parser::new(src) {
        if let Event::Start(Tag::Image(dest, _)) = event {
            found.push(dest.into_owned());
        }
    }

    let mut assets = Vec::new();

    // :TODO: create only if necessary
    std::fs::create_dir_all(cache_dir)?;

    for link in found {
        let filename = match Url::parse(&link) {
            Ok(url) => {
                let destination_path = external_resource_filepath(cache_dir, &url)?;

                if !destination_path.exists() {
                    info!("downloading {} to '{}'", url, destination_path.display());

                    if url.scheme() == "file" {
                        std::fs::copy(url.path(), &destination_path)?;
                    } else {
                        let mut response = reqwest::get(url)?;
                        let mut dest = File::create(&destination_path)?;
                        copy(&mut response, &mut dest)?;
                    }
                } else {
                    debug!(
                        "asset at {} already downloaded to '{}'",
                        url,
                        destination_path.display()
                    );
                }

                destination_path.canonicalize().context(format!(
                    "Unable to fetch the canonical path for {}",
                    destination_path.display()
                ))?
            }
            Err(_) => {
                let link = PathBuf::from(link);
                let filename = parent_dir.join(link);
                filename.canonicalize().context(format!(
                    "Unable to fetch the canonical path for {}",
                    filename.display()
                ))?
            }
        };

        if !filename.is_file() {
            return Err(failure::err_msg(format!(
                "Asset was not a file, {}",
                filename.display()
            )));
        }

        assets.push(filename);
    }

    Ok(assets)
}

/// Return the filepath where an external resource is/will be downloaded / cached.
/// The filename will have the following form `$cache_dir/$hash_of_url.$ext`
pub fn external_resource_filepath<P: AsRef<Path>>(
    cache_dir: P,
    url: &Url,
) -> Result<PathBuf, Error> {
    let mut s = DefaultHasher::new();
    url.hash(&mut s);
    let hash = s.finish();

    let file = url
        .path_segments()
        .and_then(std::iter::Iterator::last)
        .unwrap();
    let extension = Path::new(file).extension().unwrap_or_default();
    let filename = cache_dir
        .as_ref()
        .join(hash.to_string())
        .with_extension(extension);

    Ok(filename)
}

#[cfg(test)]
mod tests {
    extern crate tempdir;

    use self::tempdir::TempDir;
    use super::*;

    #[test]
    fn find_images() {
        let parent_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dummy/src");
        let src =
            "![Image 1](./rust-logo.png)\n[a link](to/nowhere) ![Image 2][2]\n\n[2]: reddit.svg\n";
        let should_be = vec![
            parent_dir.join("rust-logo.png").canonicalize().unwrap(),
            parent_dir.join("reddit.svg").canonicalize().unwrap(),
        ];

        let got = assets_in_markdown(src, &parent_dir, &Path::new("")).unwrap();

        assert_eq!(got, should_be);
    }

    #[test]
    fn find_remote_image() {
        let cache_dir = TempDir::new("cache").unwrap();

        let parent_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dummy/src");
        let filepath = parent_dir.join("rust-logo.png").canonicalize().unwrap();

        // using the `file` scheme will take the same code path as a remote path (e.g. http)
        let url = format!("file://{}", filepath.display());
        let src = format!("![Image 1]({})\n", url);
        let got = assets_in_markdown(&src, &parent_dir, cache_dir.path()).unwrap();

        let should_be =
            vec![
                external_resource_filepath(cache_dir.path(), &Url::parse(&url).unwrap())
                    .unwrap()
                    .canonicalize()
                    .unwrap(),
            ];

        assert_eq!(got, should_be);
        assert!(got[0].exists());
        // :TODO: check src was updated to point to cache
    }
}
