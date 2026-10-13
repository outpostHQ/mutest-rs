#![crate_type = "lib"]

pub mod result {
    pub enum SdkError<E, R> {
        ServiceError(E),
        Response { raw: R },
        Timeout,
    }

    impl<E, R> SdkError<E, R> {
        pub fn service(err: E) -> Self { SdkError::ServiceError(err) }
    }

    impl<E, R> Default for SdkError<E, R> {
        fn default() -> Self { SdkError::Timeout }
    }
}
