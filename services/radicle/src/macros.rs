macro_rules! or_print_and_exit_with_error {
    ($result:expr) => {{
        match $result {
            Ok(val) => val,
            Err(err) => {
                eprintln!("Error: {}", err);
                std::process::exit(-1)
            }
        }
    }};
}
