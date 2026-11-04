//! Shared UI components.

pub mod api_keys;
pub mod books;
pub mod feedback;
pub mod focus;
pub mod history_table;
pub mod icon;
pub mod interactive_search;
pub mod layout;
pub mod library_add;
pub mod library_bulk;
pub mod library_common;
pub mod library_dash;
pub mod library_forms;
pub mod library_import;
pub mod library_media;
pub mod manual_import;
pub mod media_card;
pub mod modal;
pub mod music;
pub mod poster;
pub mod provider_form;
pub mod radio;
pub mod request_drawer;
pub mod request_modal;
pub mod request_status;
pub mod settings_draft;
pub mod sidebar;
pub mod toast;
pub mod topbar;

pub use api_keys::ApiKeysPanel;
pub use feedback::{
    Card, CopyButton, DataTable, EmptyState, ErrorBox, PageHeader, Skeleton, Spinner, StatCard, StatusBadge,
    Tone, health_label,
};
pub use icon::{Icon, Logo};
pub use layout::Shell;
pub use modal::{ConfirmDialog, Modal};
pub use toast::ToastHost;
