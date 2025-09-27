use mockall::automock;

#[automock]
pub trait VerbosePrinter {
    fn print(&self, text: &str);
    fn print_indented(&self, indent: u8, text: &str);
}

#[derive(Debug, Default)]
pub struct PlainVerbosePrinter {}

impl PlainVerbosePrinter {
    pub fn new() -> Self {
        Self {}
    }
}

impl VerbosePrinter for PlainVerbosePrinter {
    fn print(&self, text: &str) {
        self.print_indented(0, text);
    }

    fn print_indented(&self, indent: u8, text: &str) {
        let mut indentation = String::new();
        for _ in 0..indent {
            indentation += "  ";
        }
        println!("{indentation}{text}");
    }
}

#[derive(Debug, Default)]
pub struct SilentVerbosePrinter {}

impl SilentVerbosePrinter {
    pub fn new() -> Self {
        Self {}
    }
}

impl VerbosePrinter for SilentVerbosePrinter {
    fn print(&self, _text: &str) {}
    fn print_indented(&self, _indent: u8, _text: &str) {}
}
