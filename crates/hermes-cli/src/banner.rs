pub fn startup_banner() -> &'static str {
    "Hermes Agent Ultra"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_banner_names_ultra_runtime() {
        assert_eq!(startup_banner(), "Hermes Agent Ultra");
    }
}
