//! Who a Wayland client is, decided by how it connected and not by what it
//! says about itself.
//!
//! Two classes:
//!
//! - **Trusted**: connected to Evo's own socket. That is the shell's vela,
//!   root-run tools (the serial harness's `grim`), and anything geckoOS-boot
//!   starts. They are the system.
//! - **App**: connected through a `wp_security_context_v1` listener socket.
//!   Nova starts every app through `pyxis-wlctx`, which makes that socket
//!   (sandbox engine `pyxis`, app id, instance id) and points the app's
//!   `WAYLAND_DISPLAY` at it. The compositor attaches the metadata to the
//!   connection itself, so an app cannot change it and cannot ask for another.
//!
//! The xdg `app_id` is only a claim. For an app client the identity is the
//! context's app id; the claim decides nothing. Shell roles (`bloom`,
//! `bloom-overlay`) exist only for trusted clients.
//!
//! XWayland is one client for every X11 window, so it is never trusted: X11
//! windows are apps whatever their class says.

use smithay::reexports::wayland_server::Client;

use crate::state::ClientState;

/// Whether `client` is part of the system. Anything this cannot recognise is
/// an app: a context-created connection, XWayland, or client data of another
/// type.
pub fn client_is_trusted(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(ClientState::is_trusted)
}

/// The filter for a privileged global (screen capture, clipboard managers,
/// synthetic input, security contexts, layer shell): only the system sees it.
/// A global an app can't see is one it can't bind, so there is no request
/// to refuse later.
pub fn trusted_only(client: &Client) -> bool {
    client_is_trusted(client)
}

/// Whether a window with this claimed app id, from a client of this trust,
/// plays a shell role (the desktop or its overlay). An app that claims the
/// shell's ids is an ordinary app window.
pub fn claims_shell_role(trusted: bool, claimed_app_id: Option<&str>) -> bool {
    trusted
        && matches!(
            claimed_app_id,
            Some(crate::shell::SHELL_APP_ID) | Some(crate::shell::OVERLAY_APP_ID)
        )
}

/// The identity of a window: the context's app id for an app client, else
/// what the client claims. A context with no app id gives nothing to go on,
/// so the claim stands; it only ever has app standing.
pub fn effective_app_id(context_app_id: Option<&str>, claimed: Option<String>) -> Option<String> {
    context_app_id.map(str::to_owned).or(claimed)
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    use smithay::reexports::wayland_server::Display;
    use smithay::wayland::security_context::SecurityContext;

    use super::*;

    fn client_with(
        display: &Display<()>,
        context: Option<SecurityContext>,
    ) -> Client {
        let (server_end, _client_end) = UnixStream::pair().unwrap();
        // Leak the peer: a closed peer would disconnect the client under us.
        std::mem::forget(_client_end);
        let state = ClientState {
            security_context: context,
            ..ClientState::default()
        };
        display
            .handle()
            .insert_client(server_end, Arc::new(state))
            .unwrap()
    }

    #[test]
    fn socket_clients_are_trusted_and_context_clients_are_apps() {
        let display = Display::<()>::new().unwrap();
        let direct = client_with(&display, None);
        let creator = direct.id();
        let app = client_with(
            &display,
            Some(SecurityContext {
                sandbox_engine: Some("pyxis".into()),
                app_id: Some("weather".into()),
                instance_id: Some("1".into()),
                creator_client_id: creator,
            }),
        );
        assert!(client_is_trusted(&direct));
        assert!(!client_is_trusted(&app));
    }

    #[test]
    fn privileged_globals_are_hidden_from_apps() {
        let display = Display::<()>::new().unwrap();
        let direct = client_with(&display, None);
        let app = client_with(
            &display,
            Some(SecurityContext {
                sandbox_engine: None,
                app_id: None,
                instance_id: None,
                creator_client_id: direct.id(),
            }),
        );
        // A context with no metadata at all is still an app: it is the
        // route the client took that makes it one.
        assert!(trusted_only(&direct));
        assert!(!trusted_only(&app));
    }

    #[test]
    fn unknown_client_data_is_not_trusted() {
        use smithay::reexports::wayland_server::backend::ClientData;
        struct Other;
        impl ClientData for Other {}
        let display = Display::<()>::new().unwrap();
        let (server_end, _peer) = UnixStream::pair().unwrap();
        std::mem::forget(_peer);
        let client = display
            .handle()
            .insert_client(server_end, Arc::new(Other))
            .unwrap();
        assert!(!client_is_trusted(&client));
    }

    #[test]
    fn shell_roles_need_a_trusted_client() {
        assert!(claims_shell_role(true, Some("bloom")));
        assert!(claims_shell_role(true, Some("bloom-overlay")));
        assert!(!claims_shell_role(false, Some("bloom")));
        assert!(!claims_shell_role(false, Some("bloom-overlay")));
        assert!(!claims_shell_role(true, Some("files")));
        assert!(!claims_shell_role(true, None));
    }

    #[test]
    fn a_context_decides_an_apps_identity() {
        assert_eq!(
            effective_app_id(Some("weather"), Some("bloom-overlay".into())).as_deref(),
            Some("weather")
        );
        assert_eq!(
            effective_app_id(None, Some("bloom".into())).as_deref(),
            Some("bloom")
        );
        assert_eq!(effective_app_id(None, None), None);
    }
}
