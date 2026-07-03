//! Console and input abstractions.

/// Output abstraction. CLI prints to stdout/stderr, TUI renders to log area.
pub trait Console {
    fn print_line(&mut self, text: &str);
    fn print_error(&mut self, text: &str);
}

/// User input source. CLI reads stdin, TUI shows modal prompt.
pub trait InputSource {
    fn read_line(&mut self, prompt: &str) -> Result<String, String>;
}

/// Stdout-based console for CLI mode.
pub struct StdoutConsole;

impl Console for StdoutConsole {
    fn print_line(&mut self, text: &str) {
        println!("{text}");
    }
    fn print_error(&mut self, text: &str) {
        eprintln!("{text}");
    }
}

/// Stdin-based input for CLI mode.
pub struct StdinInput;

impl InputSource for StdinInput {
    fn read_line(&mut self, prompt: &str) -> Result<String, String> {
        use std::io::Write;
        print!("{prompt}");
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        Ok(line.trim().to_string())
    }
}

/// Collects output into a Vec — for testing.
pub struct BufferConsole {
    pub lines: Vec<String>,
}

impl Default for BufferConsole {
    fn default() -> Self {
        Self::new()
    }
}

impl BufferConsole {
    pub fn new() -> Self {
        Self { lines: Vec::new() }
    }
}

impl Console for BufferConsole {
    fn print_line(&mut self, text: &str) {
        self.lines.push(text.to_string());
    }
    fn print_error(&mut self, text: &str) {
        self.lines.push(format!("Error: {text}"));
    }
}

/// No-op input source for tests that don't need interactive prompts.
pub struct NoInput;

impl InputSource for NoInput {
    fn read_line(&mut self, _prompt: &str) -> Result<String, String> {
        Err("no input available".into())
    }
}
