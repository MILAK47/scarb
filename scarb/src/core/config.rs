use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{env, mem};

use anyhow::{anyhow, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use once_cell::sync::OnceCell;
use tracing::trace;
use which::which_in;

#[cfg(doc)]
use crate::core::Workspace;
use crate::dirs::AppDirs;
use crate::flock::{AdvisoryLock, RootFilesystem};
use crate::ui::Ui;
use crate::DEFAULT_TARGET_DIR_NAME;
use crate::SCARB_ENV;

pub struct Config {
    manifest_path: Utf8PathBuf,
    dirs: Arc<AppDirs>,
    target_dir: RootFilesystem,
    app_exe: OnceCell<PathBuf>,
    ui: Ui,
    creation_time: Instant,
    // HACK: This should be the lifetime of Config itself, but we cannot express that, so we
    //   put 'static here and transmute in getter function.
    package_cache_lock: OnceCell<AdvisoryLock<'static>>,
    scarb_log: String,
    offline: bool,
}

impl Config {
    pub fn init(
        manifest_path: Utf8PathBuf,
        dirs: AppDirs,
        ui: Ui,
        target_dir_override: Option<Utf8PathBuf>,
    ) -> Result<Self> {
        let creation_time = Instant::now();

        if tracing::enabled!(tracing::Level::TRACE) {
            for line in dirs.to_string().lines() {
                trace!("{line}");
            }
        }

        let target_dir = RootFilesystem::new_output_dir(target_dir_override.unwrap_or_else(|| {
            manifest_path
                .parent()
                .expect("parent of manifest path must always exist")
                .join(DEFAULT_TARGET_DIR_NAME)
        }));

        let dirs = Arc::new(dirs);

        let scarb_log = env::var("SCARB_LOG").unwrap_or_default();

        Ok(Self {
            manifest_path,
            dirs,
            target_dir,
            app_exe: OnceCell::new(),
            ui,
            creation_time,
            package_cache_lock: OnceCell::new(),
            scarb_log,
            offline: false,
        })
    }

    pub fn manifest_path(&self) -> &Utf8Path {
        &self.manifest_path
    }

    pub fn root(&self) -> &Utf8Path {
        self.manifest_path()
            .parent()
            .expect("parent of manifest path must always exist")
    }

    pub fn scarb_log(&self) -> &str {
        &self.scarb_log
    }

    pub fn dirs(&self) -> &AppDirs {
        &self.dirs
    }

    pub fn target_dir(&self) -> &RootFilesystem {
        &self.target_dir
    }

    pub fn app_exe(&self) -> Result<&Path> {
        self.app_exe
            .get_or_try_init(|| {
                let from_env = || -> Result<PathBuf> {
                    // Try re-using the `scarb` set in the environment already.
                    // This allows commands that use Scarb as a library to inherit
                    // (via `scarb <subcommand>`) or set (by setting `$SCARB`) a correct path
                    // to `scarb` when the current exe is not actually scarb (e.g. `scarb-*` binaries).
                    env::var_os(SCARB_ENV)
                        .map(PathBuf::from)
                        .ok_or_else(|| anyhow!("${SCARB_ENV} not set"))?
                        .canonicalize()
                        .map_err(Into::into)
                };

                let from_current_exe = || -> Result<PathBuf> {
                    // Try fetching the path to `scarb` using `env::current_exe()`.
                    // The method varies per operating system and might fail; in particular,
                    // it depends on `/proc` being mounted on Linux, and some environments
                    // (like containers or chroots) may not have that available.
                    env::current_exe()?.canonicalize().map_err(Into::into)
                };

                let from_argv = || -> Result<PathBuf> {
                    // Grab `argv[0]` and attempt to resolve it to an absolute path.
                    // If `argv[0]` has one component, it must have come from a `PATH` lookup,
                    // so probe `PATH` in that case.
                    // Otherwise, it has multiple components and is either:
                    // - a relative path (e.g., `./scarb`, `target/debug/scarb`), or
                    // - an absolute path (e.g., `/usr/local/bin/scarb`).
                    // In either case, [`Path::canonicalize`] will return the full absolute path
                    // to the target if it exists.
                    let argv0 = env::args_os()
                        .map(PathBuf::from)
                        .next()
                        .ok_or_else(|| anyhow!("no argv[0]"))?;
                    which_in(argv0, Some(self.dirs().path_env()), env::current_dir()?)
                        .map_err(Into::into)
                };

                from_env()
                    .or_else(|_| from_current_exe())
                    .or_else(|_| from_argv())
                    .context("could not get the path to scarb executable")
            })
            .map(AsRef::as_ref)
    }

    pub fn ui(&self) -> &Ui {
        &self.ui
    }

    pub fn elapsed_time(&self) -> Duration {
        self.creation_time.elapsed()
    }

    pub fn package_cache_lock<'a>(&'a self) -> &AdvisoryLock<'a> {
        // UNSAFE: These mem::transmute calls only change generic lifetime parameters.
        let static_al: &AdvisoryLock<'static> = self.package_cache_lock.get_or_init(|| {
            let not_static_al =
                self.dirs()
                    .cache_dir
                    .advisory_lock(".package-cache.lock", "package cache", self);
            unsafe { mem::transmute(not_static_al) }
        });
        let not_static_al: &AdvisoryLock<'a> = unsafe { mem::transmute(static_al) };
        not_static_al
    }

    /// States whether the _Offline Mode_ is turned on.
    ///
    /// For checking whether Scarb can communicate with the network, prefer to use
    /// [`Self::network_allowed`], as it might pull information from other sources in the future.
    pub const fn offline(&self) -> bool {
        self.offline
    }

    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    /// If `false`, Scarb should never access the network, but otherwise it should continue operating
    /// if possible.
    pub const fn network_allowed(&self) -> bool {
        !self.offline()
    }
}
