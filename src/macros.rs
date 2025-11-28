#[macro_export]
macro_rules! bail_unless {
    ($expr:expr) => {
        if !$expr {
            return false;
        }
    };
}

#[macro_export]
macro_rules! unwrap_or_bail {
    ($expr:expr) => {
        match $expr {
            Some(val) => val,
            None => return false,
        }
    };
}
