//! The `pledge` command grammar: clap parsing plus pure proposal inputs.
//!
//! `propose` runs before authorization with no imports, so everything here is
//! pure: parse argv, validate shapes and bounds, and build the capability
//! input. Relative times are syntax-checked here and resolved against the
//! broker clock in `run`.

use clap::{Parser, Subcommand};
use dekopon_provider_sdk::provider::{Bounded, Usage};

use crate::input::{
    DropInput, DueInput, DueSpec, DurationSpec, KeepInput, ListInput, MakeInput, StateFilter,
    StatsInput,
};
use crate::model::{MAX_NOTE_BYTES, MAX_TEXT_BYTES, MAX_TO_BYTES, valid_id};

/// The `pledge` command word.
#[derive(Debug, Parser)]
#[command(
    name = "pledge",
    version,
    propagate_version = true,
    about = "A ledger of promises this agent made, and whether it kept them."
)]
pub struct Args {
    /// The subcommand to propose.
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// One subcommand per capability.
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Record a promise owed to someone, due at a time.
    Make {
        /// Who the promise is owed to.
        #[arg(long)]
        to: String,
        /// When it is due: RFC 3339 or a relative duration like 2h.
        #[arg(long)]
        due: String,
        /// What was promised; piped stdin is read when absent.
        text: Vec<String>,
    },
    /// List promises, newest first, as one JSON object per line.
    List {
        /// Only promises owed to this recipient.
        #[arg(long)]
        to: Option<String>,
        /// Only open promises.
        #[arg(long, conflicts_with_all = ["settled", "kept", "dropped"])]
        open: bool,
        /// Only settled promises.
        #[arg(long, conflicts_with_all = ["open", "kept", "dropped"])]
        settled: bool,
        /// Only kept promises.
        #[arg(long, conflicts_with_all = ["open", "settled", "dropped"])]
        kept: bool,
        /// Only dropped promises.
        #[arg(long, conflicts_with_all = ["open", "settled", "kept"])]
        dropped: bool,
        /// Only open promises past their due time.
        #[arg(long)]
        overdue: bool,
        /// Maximum promises to emit.
        #[arg(long)]
        limit: Option<u32>,
        /// Oldest first instead of newest first.
        #[arg(long)]
        oldest_first: bool,
        /// Exit 1 with `none-found` when nothing matches.
        #[arg(long)]
        fail_if_none: bool,
    },
    /// Show open promises due within a window, plus overdue ones.
    Due {
        /// Window ahead of now; 24h when absent.
        #[arg(long)]
        within: Option<String>,
        /// Only promises owed to this recipient.
        #[arg(long)]
        to: Option<String>,
        /// Maximum promises to emit.
        #[arg(long)]
        limit: Option<u32>,
        /// Exit 1 with `none-due` when nothing is due, for watch probes.
        #[arg(long)]
        fail_if_none: bool,
    },
    /// Mark a promise kept.
    Keep {
        /// The promise id.
        id: String,
        /// Optional note about how it was kept.
        #[arg(long, num_args(1..))]
        note: Option<Vec<String>>,
    },
    /// Mark a promise dropped, with a reason.
    Drop {
        /// The promise id.
        id: String,
        /// Why the promise was dropped.
        #[arg(long, num_args(1..), required(true))]
        reason: Vec<String>,
    },
    /// Report kept, broken and open counts with the follow-through rate.
    Stats {
        /// Only promises owed to this recipient.
        #[arg(long)]
        to: Option<String>,
        /// One counts line per recipient, sorted by recipient.
        #[arg(long)]
        by_recipient: bool,
    },
}

fn bounded<const MAX: usize>(text: String, what: &'static str) -> Result<Bounded<MAX>, Usage> {
    Bounded::new(text).map_err(|_| Usage::new(format!("{what} exceeds {MAX} UTF-8 bytes")))
}

fn recipient(to: String) -> Result<Bounded<MAX_TO_BYTES>, Usage> {
    if to.trim().is_empty() {
        return Err(Usage::new("the recipient is missing"));
    }
    bounded(to, "the recipient")
}

fn optional_recipient(to: Option<String>) -> Result<Option<Bounded<MAX_TO_BYTES>>, Usage> {
    to.map(recipient).transpose()
}

impl Cmd {
    /// Builds the `make` input, reading piped stdin in `run` when no text is given.
    pub fn make_input(self, stdin_piped: bool) -> Result<MakeInput, Usage> {
        let Self::Make { to, due, text } = self else {
            return Err(Usage::new("not a make command"));
        };
        let due = DueSpec::new(due);
        due.check().map_err(|_| {
            Usage::new("the due time is not RFC 3339 or a duration like 30s, 30m, 2h, 3d or 2w")
        })?;
        let joined = text.join(" ");
        let text = if joined.trim().is_empty() {
            if stdin_piped {
                None
            } else {
                return Err(Usage::new("supply promise text or pipe it via stdin"));
            }
        } else {
            Some(bounded::<MAX_TEXT_BYTES>(joined, "promise text")?)
        };
        Ok(MakeInput {
            to: recipient(to)?,
            due,
            text,
        })
    }

    /// Builds the `list` input.
    pub fn list_input(self) -> Result<ListInput, Usage> {
        let Self::List {
            to,
            open,
            settled,
            kept,
            dropped,
            overdue,
            limit,
            oldest_first,
            fail_if_none,
        } = self
        else {
            return Err(Usage::new("not a list command"));
        };
        if limit == Some(0) {
            return Err(Usage::new("the limit must be positive"));
        }
        let state = match (open, settled, kept, dropped) {
            (false, false, false, false) => None,
            (true, false, false, false) => Some(StateFilter::Open),
            (false, true, false, false) => Some(StateFilter::Settled),
            (false, false, true, false) => Some(StateFilter::Kept),
            (false, false, false, true) => Some(StateFilter::Dropped),
            _ => {
                return Err(Usage::new(
                    "--open, --settled, --kept and --dropped conflict",
                ));
            }
        };
        Ok(ListInput {
            to: optional_recipient(to)?,
            state,
            limit,
            oldest_first,
            overdue_only: overdue,
            fail_if_none,
        })
    }

    /// Builds the `due` input.
    pub fn due_input(self) -> Result<DueInput, Usage> {
        let Self::Due {
            within,
            to,
            limit,
            fail_if_none,
        } = self
        else {
            return Err(Usage::new("not a due command"));
        };
        if limit == Some(0) {
            return Err(Usage::new("the limit must be positive"));
        }
        let within = within
            .map(DurationSpec::new)
            .map(|spec| {
                spec.check().map(|()| spec).map_err(|_| {
                    Usage::new("the window is not a duration like 30s, 30m, 2h, 3d or 2w")
                })
            })
            .transpose()?;
        Ok(DueInput {
            within,
            to: optional_recipient(to)?,
            limit,
            fail_if_none,
        })
    }

    /// Builds the `keep` input.
    pub fn keep_input(self) -> Result<KeepInput, Usage> {
        let Self::Keep { id, note } = self else {
            return Err(Usage::new("not a keep command"));
        };
        if !valid_id(&id) {
            return Err(Usage::new(
                "the promise id is not p- followed by six hex characters",
            ));
        }
        let note = note
            .map(|words| bounded::<MAX_NOTE_BYTES>(words.join(" "), "the note"))
            .transpose()?;
        Ok(KeepInput { id, note })
    }

    /// Builds the `drop` input.
    pub fn drop_input(self) -> Result<DropInput, Usage> {
        let Self::Drop { id, reason } = self else {
            return Err(Usage::new("not a drop command"));
        };
        if !valid_id(&id) {
            return Err(Usage::new(
                "the promise id is not p- followed by six hex characters",
            ));
        }
        let reason = reason.join(" ");
        if reason.trim().is_empty() {
            return Err(Usage::new("a reason is required"));
        }
        Ok(DropInput {
            id,
            reason: bounded::<MAX_NOTE_BYTES>(reason, "the reason")?,
        })
    }

    /// Builds the `stats` input.
    pub fn stats_input(self) -> Result<StatsInput, Usage> {
        let Self::Stats { to, by_recipient } = self else {
            return Err(Usage::new("not a stats command"));
        };
        Ok(StatsInput {
            to: optional_recipient(to)?,
            by_recipient,
        })
    }
}
