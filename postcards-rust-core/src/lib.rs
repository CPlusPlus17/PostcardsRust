//! PostcardsRust core: shared types, SwissId login, PCC API and image helpers.
pub mod image_helper;
pub mod swissid;
pub mod types;

pub use image_helper::scale_and_convert_to_base64;
pub use swissid::{SwissIdLoginService, TokenService};
pub use types::Token;
pub use types::{
    Balance, PostcardCreatorQuota, RecipientAddress, SenderAddress, SwissPostcardCreatorApi,
};

/// User-Agent string used for all requests (simulates the PCC mobile app).
pub const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 6.0.1; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/52.0.2743.98 Mobile Safari/537.36";

/// PCC app redirect URI.
pub const REDIRECT_URI: &str = "ch.post.pcc://auth/1016c75e-aa9c-493e-84b8-4eb3ba6177ef";

/// PostCardApp client id.
pub const CLIENT_ID: &str = "ae9b9894f8728ca78800942cda638155";

/// PostCardApp client secret.
pub const CLIENT_SECRET: &str = "89ff451ede545c3f408d792e8caaddf0";

/// PCC REST API base URL.
pub const PCC_API_BASE: &str = "https://pccweb.api.post.ch/secure/api/mobile/v1/";
