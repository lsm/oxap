use std::any::TypeId;

#[test]
fn oxap_names_the_same_client_and_error_as_oxap_sdk() {
    assert_eq!(
        TypeId::of::<oxap::Client>(),
        TypeId::of::<oxap_sdk::Client>()
    );
    assert_eq!(
        TypeId::of::<oxap::ClientBuilder>(),
        TypeId::of::<oxap_sdk::ClientBuilder>()
    );
    assert_eq!(TypeId::of::<oxap::Error>(), TypeId::of::<oxap_sdk::Error>());
}
