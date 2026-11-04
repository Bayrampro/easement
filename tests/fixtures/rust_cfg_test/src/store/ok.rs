pub fn run() {}

#[cfg(test)]
mod tests {
    use testkit::Clock;

    #[test]
    fn runs() {
        let _ = Clock;
    }
}
