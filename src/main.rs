extern crate getopts;
extern crate git2;
extern crate rustc_serialize;
extern crate walkdir;
extern crate curl;
extern crate sha2;

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use git2::Repository;

use walkdir::WalkDir;
use walkdir::WalkDirIterator;

use rustc_serialize::json;

use curl::easy::Easy;

use sha2::{Digest, Sha256};

/// Exit on error, printing the given error message with identical arguments as
/// to println!
macro_rules! error {
    ($fmtstr:tt) => { error!($fmtstr,) };
    ($fmtstr:tt, $( $args:expr ),* ) => {
        {
            println!($fmtstr, $( $args ),* );
            ::std::process::exit(1);
        }
    };
}

/// Represents the config.json file in the crates.io-index
#[derive(RustcDecodable, RustcEncodable)]
struct ConfigJsonFile {
    dl: String,
    api: String,
    dl_orig: Option<String>,
}
impl ConfigJsonFile {
    /// Read the config given the path to the git directory
    fn read(git_dir: &PathBuf) -> Self {
        let mut path = git_dir.clone();
        path.push("config.json");
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                error!("Error opening file {}: {}", path.to_string_lossy(), e)
            },
        };
        let mut tmp = String::new();
        match f.read_to_string(&mut tmp) {
            Ok(_) => (),
            Err(e) => error!("Error reading {}: {}", path.to_string_lossy(), e),
        }

        match json::decode(&tmp) {
            Ok(x) => x,
            Err(e) => error!("Error parsing {}: {}", path.to_string_lossy(), e),
        }
    }
    /// Write the config.json file to the given path in the git directory
    fn write(&self, git_dir: &PathBuf) {
        let mut path = git_dir.clone();
        path.push("config.json");

        let tmp: String =
            json::encode(self).expect("Error encoding Config").to_string();

        let mut f = match File::create(&path) {
            Ok(f) => f,
            Err(e) => {
                error!("Error creating file {}: {}",
                       &path.to_string_lossy(),
                       e);
            },
        };

        match f.write_all(tmp.as_bytes()) {
            Ok(()) => (),
            Err(e) => {
                error!("Error writing to file {}: {}",
                       &path.to_string_lossy(),
                       e)
            },
        }
    }
}

/// Represents the settings in a given run of the program
#[derive(Debug)]
struct Settings {
    help: bool,
    version: bool,
    update_index: bool,
    download_yanked: bool,
    check_sums: bool,
    replace: Option<String>,
    strict_mode: bool,
    download_old: bool,
    archive: PathBuf,
    use_orig_dl: bool,
}
impl<'a> From<&'a getopts::Matches> for Settings {
    fn from(matches: &getopts::Matches) -> Self {

        let archive = match matches.free.get(0) {
            Some(x) => &x,
            None => "",
        };
        Settings {
            help: matches.opt_present("help"),
            version: matches.opt_present("version"),
            update_index: matches.opt_present("no-update-index") == false,
            download_yanked: matches.opt_present("yanked"),
            check_sums: matches.opt_present("no-check-sums") == false,
            replace: matches.opt_str("replace"),
            strict_mode: matches.opt_present("strict"),
            download_old: matches.opt_present("download-old"),
            archive: PathBuf::from(archive),
            use_orig_dl: matches.opt_present("use-orig-dl"),
        }
    }
}

/// Represents information about a single .crate file
#[derive(RustcDecodable, Debug, Eq)]
struct Crate {
    name: String,
    vers: String,
    yanked: bool,
    cksum: String,
}
impl Crate {
    fn new(name: &str, vers: &str) -> Self {
        Crate {
            name: name.to_string(),
            vers: vers.to_string(),
            yanked: true,
            cksum: String::new(),
        }
    }
    /// Return the URL which should be used to download the crate from
    fn download_url(&self, config: &ConfigJsonFile, settings: &Settings) -> String {
        if settings.use_orig_dl {
            format!("{}/{}/{}/download", config.dl, self.name, self.vers)
        } else {
            format!("https://static.crates.io/crates/{}/{}-{}.crate",
                    self.name,
                    self.name,
                    self.vers)
        }
    }
}
impl PartialEq for Crate {
    fn eq(&self, other: &Crate) -> bool {
        self.name == other.name && self.vers == other.vers
    }
}
impl Ord for Crate {
    fn cmp(&self, other: &Crate) -> Ordering {
        match self.name.cmp(&other.name) {
            Ordering::Equal => self.vers.cmp(&other.vers),
            x => x,
        }
    }
}
impl PartialOrd for Crate {
    fn partial_cmp(&self, other: &Crate) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut opts = getopts::Options::new();
    opts.optflag("", "no-update-index", "Don't update the index");
    opts.optflag("", "yanked", "Also download yanked .crate files");
    opts.optflag("",
                 "no-check-sums",
                 "Don't verify the checksums of downloaded .crate files");
    opts.optopt("",
                "replace",
                "Specify the URL to replace the index repository dl url",
                "URL");
    opts.optflag("", "strict", "exit immediately on any error/checksum mismatch");
    opts.optflag("", "download-old", "download old versions of crates, default is to only download newest version of every crate");
    opts.optflag("", "use-orig-dl", "download from the URL specified in the upstream index repository. May help if unable to download crates, but will likely cause the download counter to be incremented and should normally not be used.");
    opts.optflag("h", "help", "print the help menu");
    opts.optflag("", "version", "print program version");

    let matches = match opts.parse(&args[1..]) {
        Ok(x) => x,
        Err(e) => error!("Error parsing options: {}", e.description()),
    };

    let settings = Settings::from(&matches);

    if settings.help {
        let brief = "Usage: crates-ectype [options] ARCHIVE-DIRECTORY";
        print!("{}", opts.usage(&brief));
        return;
    }

    if settings.version {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        println!("{}", env!("CARGO_PKG_HOMEPAGE"));
        return;
    }

    match matches.free.len() {
        0 => error!("You must specify an archive location."),
        1 => (),
        _ => error!("You cannot specify more than one archive location."),
    }

    create_dir(&settings.archive);

    let mut git_dir = settings.archive.clone();
    git_dir.push("index");

    if settings.update_index {
        update_git_repo(&git_dir,
                        "https://github.com/rust-lang/crates.io-index");
    }

    let config = ConfigJsonFile::read(&git_dir);

    let crates = read_crate_index(&git_dir, &settings);

    fetch_crates(&crates,
                 &config,
                 &settings);

    if let Some(new_url) = settings.replace {
        replace_url(&new_url, &git_dir);
    }
}

fn create_dir(path: &PathBuf) {
    if path.is_dir() == false {
        if path.exists() {
            error!("File already exists: {}", path.to_string_lossy());
        } else {
            match fs::create_dir(path) {
                Ok(()) => (),
                Err(e) => {
                    error!("Error creating directory {}: {}",
                           path.to_string_lossy(),
                           e)
                },
            }
        }
    }
}

fn update_git_repo(git_dir: &PathBuf, url: &str) {
    let path = git_dir.as_os_str();

    if git_dir.is_dir() {
        match Repository::open(path) {
            Ok(mut x) => {
                git_pull(&mut x);
                x
            },
            Err(e) => {
                error!("Error opening index repository at {}: {}",
                       git_dir.to_string_lossy(),
                       e)
            },
        }
    } else {
        println!("Cloning index directory into {}", git_dir.to_string_lossy());
        match Repository::clone(url, path) {
            Ok(x) => {
                println!("Done cloning index directory");
                x
            },
            Err(e) => error!("Error cloning index repository: {}", e),
        }
    };
}

/// Equivalent to doing git pull on the crates.io-index repository
fn git_pull(repo: &mut Repository) {
    println!("Updating index repository");
    let remote = match repo.remotes() {
        Ok(ref remotes) if remotes.len() == 0 => {
            error!("index repository has zero remotes");
        },
        Ok(ref remotes) if remotes.len() == 1 => {
            remotes.get(0).expect("git_pull index error").to_string()
        },
        Ok(_) => {
            error!("index has more than 1 remote");
        },
        Err(e) => {
            error!("index error getting remotes: {}", e);
        },
    };
    let mut remote =
        repo.find_remote(&remote).expect("git_pull error getting remote");

    match remote.fetch(&[], None, None) {
        Ok(()) => (),
        Err(e) => error!("index error fetching from remote: {}", e),
    }

    let oid = match repo.refname_to_id("refs/remotes/origin/master") {
        Ok(x) => x,
        Err(e) => error!("Error getting refs/remotes/origin/master ref: {}", e),
    };
    let object =
        repo.find_object(oid, None).expect("git_pull error getting object");
    repo.reset(&object, git2::ResetType::Hard, None)
        .expect("git_pull error doing hard reset");

    println!("Done updating index repository");
}

/// Read the index directory, returning all the Crates
fn read_crate_index(git_dir: &PathBuf,
                    settings: &Settings)
                    -> BTreeSet<Crate> {
    println!("Reading the crates index");
    let mut ret = BTreeSet::new();

    for file in WalkDir::new(&git_dir)
            .into_iter()
            .filter_entry(|e| {
        let filename = match e.file_name().to_str() {
            Some(x) => x,
            None => return false,
        };
        if filename.starts_with(".") || filename == "config.json" {
            false
        } else {
            true
        }
    })
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file()) {
        /* Iterate over all files in the index, skipping
         * config.json */
        let f = match File::open(file.path()) {
            Ok(f) => f,
            Err(e) => {
                error!("Error opening file {}: {}", file.path().display(), e)
            },
        };
        let f = BufReader::new(f);

        let mut iter = f.lines().peekable();
        loop {
            let line = match iter.next() {
                Some(x) => x,
                None => break,
            };

            let line = match line {
                Ok(x) => x,
                Err(e) => {
                    error!("read_crate_index error reading line in {}: {}",
                           file.path().display(),
                           e)
                },
            };
            let crate_info: Crate = match json::decode(&line) {
                Ok(x) => x,
                Err(e) => {
                    error!("Error parsing json in {}: {}",
                           file.path().display(),
                           e)
                },
            };

            /* Assume that the newest version is listed last in the index file */
            if (settings.download_yanked || crate_info.yanked == false)
                && (settings.download_old || iter.peek().is_none()) {
                ret.insert(crate_info);
            }
        }
    }

    println!("Finished reading crates index");
    println!("Found info for {} .crate files", ret.len());

    /* The following crates are unavailable for unknown reasons, so we
     * remove them, since trying to download them results in an error */
    let unavailable_crates =
        vec![Crate::new("STD", "0.1.0"),
             Crate::new("glib-2-0-sys", "0.0.1"),
             Crate::new("glib-2-0-sys", "0.0.2"),
             Crate::new("glib-2-0-sys", "0.0.3"),
             Crate::new("glib-2-0-sys", "0.0.4"),
             Crate::new("glib-2-0-sys", "0.0.5"),
             Crate::new("glib-2-0-sys", "0.0.6"),
             Crate::new("glib-2-0-sys", "0.0.7"),
             Crate::new("glib-2-0-sys", "0.0.8"),
             Crate::new("glib-2-0-sys", "0.1.0"),
             Crate::new("glib-2-0-sys", "0.1.1"),
             Crate::new("glib-2-0-sys", "0.1.2"),
             Crate::new("glib-2-0-sys", "0.2.0"),
             Crate::new("gobject-2-0-sys", "0.0.2"),
             Crate::new("gobject-2-0-sys", "0.0.3"),
             Crate::new("gobject-2-0-sys", "0.0.4"),
             Crate::new("gobject-2-0-sys", "0.0.5"),
             Crate::new("gobject-2-0-sys", "0.0.6"),
             Crate::new("gobject-2-0-sys", "0.0.7"),
             Crate::new("gobject-2-0-sys", "0.0.8"),
             Crate::new("gobject-2-0-sys", "0.0.9"),
             Crate::new("gobject-2-0-sys", "0.1.0"),
             Crate::new("gobject-2-0-sys", "0.2.0"),
             Crate::new("ojfiewijogwhiogerhiugerhiuegr", "0.1.0"),
             Crate::new("ojfiewijogwhiogerhiugerhiuegr", "0.1.1"),
             Crate::new("ojfiewijogwhiogerhiugerhiuegr", "0.1.2"),
             Crate::new("rustbook", "0.1.0"),
             Crate::new("rustbook", "0.2.0"),
             Crate::new("rustbook", "0.3.0"),
             Crate::new("cargo-ctags", "0.2.3"),
             Crate::new("wright", "0.2.2"), /* https://github.com/rust-lang/crates.io/issues/1201 */
             Crate::new("stitch", "0.1.0"), /* https://github.com/C4K3/crates-ectype/issues/1 */
             ];

    for c in &unavailable_crates {
        let _: bool = ret.remove(c);
    }

    ret
}

fn fetch_crates(crates: &BTreeSet<Crate>,
                config: &ConfigJsonFile,
                settings: &Settings) {
    let crates_dir = &settings.archive;

    let mut output = Vec::new();
    let mut handle = Easy::new();
    handle
        .follow_location(true)
        .expect("fetch_crates error setting follow_location to true");
    handle
        .fail_on_error(true)
        .expect("fetch_crates error setting fail_on_error to true");

    /* A list of downloaded crates whose checksums did not match */
    let mut checksum_mismatches = Vec::new();

    for c in crates {
        let crate_name = format!("{}-{}.crate", c.name, c.vers);
        let cratefile = crates_dir.join(&crate_name);
        if cratefile.exists() {
            if settings.check_sums {
                /* Check the downloaded file matches the sha256 hash in the
                 * registry */
                output.clear();
                let mut f = match File::open(&cratefile) {
                    Ok(f) => f,
                    Err(e) => {
                        error!("Error opening {}: {}",
                               cratefile.to_string_lossy(),
                               e)
                    },
                };
                match f.read_to_end(&mut output) {
                    Ok(_) => (),
                    Err(e) => {
                        error!("Error reading {}: {}",
                               cratefile.to_string_lossy(),
                               e)
                    },
                };
                let hash = sha256sum(&output);
                if hash != c.cksum {
                    error!("Checksum mismatch in {}. Expected {} but file's sha256sum is {}",
                           cratefile.to_string_lossy(),
                           c.cksum,
                           hash);
                }
            }
            continue;
        }

        let partfile = crates_dir.join(&format!("{}.part", crate_name));
        let url = c.download_url(&config, settings);
        println!("Fetching {} version {} from {}", c.name, c.vers, url);

        handle.url(&url).expect("fetch_crates error setting url");

        /* Reuse the same vector */
        output.clear();
        {
            let mut transfer = handle.transfer();
            transfer
                .write_function(|new_data| {
                                    output.extend_from_slice(new_data);
                                    Ok(new_data.len())
                                })
                .expect("fetch_crates error setting write_function");

            match transfer.perform() {
                Ok(()) => (),
                Err(e) => error!("Error downloading {}: {}", crate_name, e),
            }
        }

        let hash = sha256sum(&output);
        /* That there is the hash of the crate not found error message.
         * Unfortunately crates.io returns 200 even when the crate can't be
         * found, so this is an easy way of checking if the crate was not
         * found */
        if &hash ==
           "59d2652e67d6af1844f035488a12ecdd3c680554eff0bf982aad28814b5963a9" {
            error!("Warning: crate {}-{} could not be downloaded!",
                   c.name,
                   c.vers);
        }
        if hash != c.cksum {
            /* Check the downloaded file matches the sha256 hash in the
             * registry */
            if settings.strict_mode {
                error!("Checksum mismatch in {}-{}. Expected hash {} but received file with hash {}",
                       c.name,
                       c.vers,
                       c.cksum,
                       hash);
            } else {
                checksum_mismatches.push((c, hash));
                continue;
            }
        }

        let mut f = match File::create(&partfile) {
            Ok(f) => f,
            Err(e) => {
                error!("Error creating file {}: {}",
                       partfile.to_string_lossy(),
                       e)
            },
        };

        match f.write_all(&output) {
            Ok(()) => (),
            Err(e) => {
                error!("Error writing to {}: {}", partfile.to_string_lossy(), e)
            },
        }

        // let partfile = crates_dir.join(&format!("{}.part", crate_name));
        match fs::rename(&partfile, &cratefile) {
            Ok(()) => (),
            Err(e) => {
                error!("Error renaming {} to {}: {}",
                       partfile.to_string_lossy(),
                       cratefile.to_string_lossy(),
                       e)
            },
        }
    }

    if !settings.strict_mode {
        if !checksum_mismatches.is_empty() {
            println!("Warning: The following {} crates were not saved because their checksum did not match the checksum in the index:",
                      checksum_mismatches.len());
        }
        for (c, downloaded_hash) in checksum_mismatches {
            println!("	{}-{} expected hash {} but received file with hash {}",
                      c.name,
                      c.vers,
                      c.cksum,
                      downloaded_hash);

        }
    }
}

fn replace_url(new_url: &str, git_dir: &PathBuf) {
    /* First we edit the actual file (if need be) */
    let mut config = ConfigJsonFile::read(git_dir);

    if new_url == config.dl {
        return;
    }

    let dl_orig = if let Some(x) = config.dl_orig {
        x
    } else {
        config.dl
    };

    config.dl = new_url.to_string();
    config.dl_orig = Some(dl_orig);

    config.write(git_dir);

    /* Now we commit the changes */
    let repo = match Repository::open(git_dir) {
        Ok(x) => x,
        Err(e) => {
            error!("Error opening index repository at {}: {}",
                   git_dir.to_string_lossy(),
                   e)
        },
    };

    let mut index = repo.index().expect("Error getting repo index");

    /* git add config.json */
    let config_path = Path::new("config.json");
    index.add_path(&config_path).expect("Error adding path to repo index");
    index.write().expect("Error writing repo index");
    let tree_id = index.write_tree().expect("Error writing repo index tree");

    /* git commit -m "crates-ectype updating DL location" */
    let tree = repo.find_tree(tree_id).expect("Error getting tree");
    let head = repo.head()
        .expect("Error getting repo head")
        .target()
        .expect("Error getting repo head target");
    let parent = repo.find_commit(head).expect("Error getting head commit");
    let sig = git2::Signature::now("crates-ectype", "no-email").expect("Error creating git signature");
    repo.commit(Some("HEAD"),
                &sig,
                &sig,
                "crates-ectype updating DL location",
                &tree,
                &[&parent])
        .expect("Error committing URL update");

    println!("Replaced DL url with {}", new_url);
}

/// Calculate the sha256sum of the data, returning it as a hex string
fn sha256sum(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.input(data);
    hasher
        .result()
        .iter()
        .map(|x| format!("{:02x}", x))
        .fold("".to_string(), |mut a, b| {
            a.push_str(&b);
            a
        })
}
