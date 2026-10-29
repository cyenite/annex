use std::env;
use std::time::{Duration, Instant};

use console::{Term, style};
use indicatif::{ProgressBar, ProgressStyle};

const TICK: Duration = Duration::from_millis(80);
const OUTPUT_LINES: usize = 12;

/// How a failed step is reported: what ran, what it said, and what to do about it.
pub struct Failure {
    pub output: String,
    pub message: String,
    pub fix: &'static str,
    pub command: Option<String>,
}

/// Setup's terminal output: spinners, a byte progress bar and colour on a terminal, plain lines
/// when piped or when NO_COLOR is set.
pub struct Ui {
    is_fancy: bool,
}

impl Ui {
    pub fn new() -> Self {
        let is_fancy = Term::stdout().is_term() && env::var_os("NO_COLOR").is_none();
        if !is_fancy {
            console::set_colors_enabled(false);
        }
        Self { is_fancy }
    }

    pub fn heading(&self, text: &str) {
        println!("{}", style(text).bold());
    }

    pub fn item(&self, text: &str) {
        println!("  {} {text}", style("•").dim());
    }

    pub fn note(&self, text: &str) {
        println!("{}", style(text).dim());
    }

    pub fn start(&self, title: &str) -> Task {
        let bar = if self.is_fancy {
            let bar = ProgressBar::new_spinner().with_style(spinner_style());
            bar.set_message(title.to_string());
            bar.enable_steady_tick(TICK);
            Some(bar)
        } else {
            println!("... {title}");
            None
        };
        Task {
            bar,
            title: title.to_string(),
            started: Instant::now(),
            reported_tenth: 0,
        }
    }
}

pub struct Task {
    bar: Option<ProgressBar>,
    title: String,
    started: Instant,
    /// The last tenth of a download reported in plain mode, so piped output stays short.
    reported_tenth: u64,
}

impl Task {
    /// Shows what the step is doing right now, after its title.
    pub fn detail(&self, text: &str) {
        match &self.bar {
            Some(bar) => bar.set_message(format!("{}  {}", self.title, style(text).dim())),
            None => println!("    {text}"),
        }
    }

    /// Switches the spinner to a byte progress bar. `total` may grow as a download learns sizes.
    pub fn bytes(&mut self, done: u64, total: u64) {
        match &self.bar {
            Some(bar) => {
                if bar.length().is_none() {
                    bar.set_style(bytes_style());
                }
                bar.set_length(total.max(1));
                bar.set_position(done.min(total));
            }
            None if total > 0 => {
                let tenth = done * 10 / total;
                if tenth > self.reported_tenth {
                    self.reported_tenth = tenth;
                    println!("    {}% of {}", tenth * 10, indicatif::HumanBytes(total));
                }
            }
            None => {}
        }
    }

    pub fn succeed(self) {
        let line = format!(
            "{} {}  {}",
            style("✔").green().bold(),
            self.title,
            style(elapsed(self.started)).dim()
        );
        self.finish(&line, "[ok]");
    }

    pub fn needs_you(self) {
        let line = format!("{} {}", style("!").yellow().bold(), self.title);
        self.finish(&line, "[needs you]");
    }

    pub fn fail(self, failure: &Failure) {
        let line = format!(
            "{} {}  {}",
            style("✖").red().bold(),
            self.title,
            style(elapsed(self.started)).dim()
        );
        self.finish(&line, "[failed]");
        println!("    {}", style(&failure.message).red());
        if let Some(command) = &failure.command {
            println!("    {} {command}", style("command:").dim());
        }
        let output: Vec<&str> = failure.output.lines().collect();
        if !output.is_empty() {
            println!("    {}", style("last output:").dim());
            for line in &output[output.len().saturating_sub(OUTPUT_LINES)..] {
                println!("      {line}");
            }
        }
        println!("    {} {}", style("fix:").bold(), failure.fix);
    }

    fn finish(self, fancy: &str, plain: &str) {
        match self.bar {
            Some(bar) => {
                bar.finish_and_clear();
                println!("{fancy}");
            }
            None => println!("{plain} {} ({})", self.title, elapsed(self.started)),
        }
    }
}

fn elapsed(started: Instant) -> String {
    format!("{:.1} s", started.elapsed().as_secs_f64())
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan} {msg}  {elapsed:.dim}")
        .expect("valid template")
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
}

fn bytes_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.cyan} {msg}\n  [{bar:32.cyan/dim}] {bytes}/{total_bytes}  {bytes_per_sec}  ETA {eta}",
    )
    .expect("valid template")
    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
    .progress_chars("━╸ ")
}
