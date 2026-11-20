// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#define _GNU_SOURCE
#include <gtk/gtk.h>
#include <webkit/webkit.h>
#include <glib-unix.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <errno.h>
#include "host-contract.h"

typedef struct {
    GMainLoop *loop;
    GtkWindow *window;
    WebKitWebView *view;
    GSocket *refusal_socket;
    int port;
    gboolean pending;
} Journal;

static gboolean journal_uri(Journal *journal, GUri *uri)
{
    return uri && !g_uri_get_userinfo(uri) &&
        g_strcmp0(g_uri_get_scheme(uri), "http") == 0 &&
        g_strcmp0(g_uri_get_host(uri), "127.0.0.1") == 0 &&
        g_uri_get_port(uri) == journal->port;
}

static gboolean external_uri(GUri *uri)
{
    return uri && g_uri_get_host(uri) && !g_uri_get_userinfo(uri) &&
        (g_strcmp0(g_uri_get_scheme(uri), "http") == 0 ||
         g_strcmp0(g_uri_get_scheme(uri), "https") == 0);
}

static void answer(Journal *journal, gboolean ok)
{
    if (journal->pending) {
        journal->pending = FALSE;
        if (write(STDOUT_FILENO, ok ? "+" : "-", 1) != 1 || !ok)
            g_main_loop_quit(journal->loop);
    }
}

static void open_with_default_handler(const char *url)
{
    g_autoptr(GAppLaunchContext) launch = g_app_launch_context_new();
    g_app_launch_context_unsetenv(launch, "LD_LIBRARY_PATH");
    g_app_launch_context_unsetenv(launch, "SOLSTONE_JOURNAL_LANGUAGE");
    g_app_launch_context_unsetenv(launch, "SOLSTONE_JOURNAL_NEW_NAME");
    g_app_info_launch_default_for_uri_async(url, launch, NULL, NULL, NULL);
}

static void portal_opened(GObject *source, GAsyncResult *result, gpointer data)
{
    g_autofree char *url = data;
    g_autoptr(GError) error = NULL;
    g_autoptr(GVariant) handle = g_dbus_connection_call_finish(G_DBUS_CONNECTION(source), result, &error);
    if (!handle && (g_error_matches(error, G_DBUS_ERROR, G_DBUS_ERROR_SERVICE_UNKNOWN) ||
                    g_error_matches(error, G_DBUS_ERROR, G_DBUS_ERROR_NAME_HAS_NO_OWNER) ||
                    g_error_matches(error, G_DBUS_ERROR, G_DBUS_ERROR_UNKNOWN_METHOD) ||
                    g_error_matches(error, G_DBUS_ERROR, G_DBUS_ERROR_UNKNOWN_INTERFACE)))
        open_with_default_handler(url);
}

static void open_external(const char *url)
{
    // The desktop launches the owner's handler outside the viewer's namespace
    // and AppArmor profile. Older desktops without the portal can use GIO.
    g_autoptr(GDBusConnection) bus = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, NULL);
    if (!bus) {
        open_with_default_handler(url);
        return;
    }
    g_dbus_connection_call(bus, "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop", "org.freedesktop.portal.OpenURI", "OpenURI",
        g_variant_new("(ss@a{sv})", "", url, g_variant_new_array(G_VARIANT_TYPE("{sv}"), NULL, 0)),
        G_VARIANT_TYPE("(o)"), G_DBUS_CALL_FLAGS_NONE, 5000, NULL, portal_opened, g_strdup(url));
}

static gboolean decide(WebKitWebView *view, WebKitPolicyDecision *decision,
                       WebKitPolicyDecisionType type, gpointer data)
{
    Journal *journal = data;
    if (type == WEBKIT_POLICY_DECISION_TYPE_RESPONSE) {
        WebKitResponsePolicyDecision *response = WEBKIT_RESPONSE_POLICY_DECISION(decision);
        WebKitURIResponse *item = webkit_response_policy_decision_get_response(response);
        g_autoptr(GUri) uri = g_uri_parse(webkit_uri_response_get_uri(item), G_URI_FLAGS_NONE, NULL);
        if (!journal_uri(journal, uri)) {
            webkit_policy_decision_ignore(decision);
            return TRUE;
        }
        const char *path = g_uri_get_path(uri);
        guint status = webkit_uri_response_get_status_code(item);
        if (status >= 200 && status < 300 &&
            g_strcmp0(path, "/app/import/api/journal-archive/export") == 0 &&
            g_strcmp0(webkit_uri_response_get_mime_type(item), "application/zip") == 0) {
            webkit_policy_decision_download(decision);
            return TRUE;
        }
        if (webkit_response_policy_decision_is_main_frame_main_resource(response) &&
            journal->pending && status >= 500) {
            webkit_policy_decision_ignore(decision);
            answer(journal, FALSE);
            return TRUE;
        }
        return FALSE;
    }
    WebKitNavigationAction *action = webkit_navigation_policy_decision_get_navigation_action(
        WEBKIT_NAVIGATION_POLICY_DECISION(decision));
    const char *url = webkit_uri_request_get_uri(webkit_navigation_action_get_request(action));
    g_autoptr(GUri) uri = g_uri_parse(url, G_URI_FLAGS_NONE, NULL);
    if (journal_uri(journal, uri)) {
        if (type != WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION)
            return FALSE;
        webkit_policy_decision_ignore(decision);
        webkit_web_view_load_uri(view, url);
    } else {
        webkit_policy_decision_ignore(decision);
        if (!journal->pending && external_uri(uri) && webkit_navigation_action_is_user_gesture(action))
            open_external(url);
    }
    return TRUE;
}

static void loaded(WebKitWebView *view, WebKitLoadEvent event, gpointer data)
{
    Journal *journal = data;
    if (event != WEBKIT_LOAD_COMMITTED || !journal->pending)
        return;
    g_autoptr(GUri) uri = g_uri_parse(webkit_web_view_get_uri(view), G_URI_FLAGS_NONE, NULL);
    if (journal_uri(journal, uri) && strcmp(g_uri_get_path(uri), "/_bridge/bootstrap") != 0) {
        gtk_window_present(journal->window);
        answer(journal, TRUE);
    }
}

static gboolean failed(WebKitWebView *view, WebKitLoadEvent event, const char *uri,
                       GError *error, gpointer data)
{
    (void)view; (void)event; (void)uri;
    if (!g_error_matches(error, WEBKIT_NETWORK_ERROR, WEBKIT_NETWORK_ERROR_CANCELLED))
        answer(data, FALSE);
    return FALSE;
}

static void terminated(WebKitWebView *view, WebKitWebProcessTerminationReason reason, gpointer data)
{
    (void)view; (void)reason;
    Journal *journal = data;
    answer(journal, FALSE);
    g_main_loop_quit(journal->loop);
}

static gboolean closed(GtkWindow *window, gpointer data)
{
    (void)window;
    g_main_loop_quit(((Journal *)data)->loop);
    return FALSE;
}

static gboolean permission(WebKitWebView *view, WebKitPermissionRequest *request, gpointer unused)
{
    (void)view; (void)unused;
    webkit_permission_request_deny(request);
    return TRUE;
}

typedef struct {
    char *staged;
    char *destination;
    gboolean replacing;
    gboolean failed;
} PendingSave;

static void free_save(gpointer data)
{
    PendingSave *save = data;
    if (save->staged)
        unlink(save->staged);
    g_free(save->staged);
    g_free(save->destination);
    g_free(save);
}

static void download_failed(WebKitDownload *download, GError *error, gpointer data)
{
    (void)download; (void)error;
    ((PendingSave *)data)->failed = TRUE;
}

static guint64 identity_length(WebKitDownload *download)
{
    WebKitURIResponse *response = webkit_download_get_response(download);
    SoupMessageHeaders *headers = response ? webkit_uri_response_get_http_headers(response) : NULL;
    const char *encoding = headers ? soup_message_headers_get_one(headers, "Content-Encoding") : NULL;
    // WebKit saves decoded bytes. Only an identity response has a wire length
    // which can prove the staged file is complete before replacing an original.
    if (!response || (encoding && g_ascii_strcasecmp(encoding, "identity")))
        return 0;
    return webkit_uri_response_get_content_length(response);
}

static void download_finished(WebKitDownload *download, gpointer data)
{
    PendingSave *save = data;
    guint64 expected = identity_length(download);
    if (expected && save->staged) {
        struct stat staged;
        if (stat(save->staged, &staged) < 0 || (guint64)staged.st_size != expected)
            save->failed = TRUE;
    }
    if (!save->failed && save->staged) {
        // Both names are on the chosen filesystem. A failed or cancelled
        // transfer never touches the existing file. A new save cannot clobber
        // a file created while its transfer was running.
        int result = save->replacing ? rename(save->staged, save->destination) :
            renameat2(AT_FDCWD, save->staged, AT_FDCWD, save->destination, RENAME_NOREPLACE);
        if (result == -1)
            g_warning("journal download could not be saved: %s", g_strerror(errno));
    }
    if (save->staged)
        unlink(save->staged);
    g_clear_pointer(&save->staged, g_free);
}

static void show_save_dialog(WebKitDownload *, GtkWindow *, const char *, GFile *, gboolean);

static void save_response(GtkNativeDialog *dialog, int response, gpointer data)
{
    WebKitDownload *download = WEBKIT_DOWNLOAD(data);
    g_autoptr(GFile) file = response == GTK_RESPONSE_ACCEPT ?
        gtk_file_chooser_get_file(GTK_FILE_CHOOSER(dialog)) : NULL;
    g_autofree char *path = file ? g_file_get_path(file) : NULL;
    PendingSave *save = g_object_get_data(G_OBJECT(download), "journal-save");
    if (path) {
        struct stat original;
        save->replacing = lstat(path, &original) == 0;
        if (save->replacing && !identity_length(download)) {
            // If WebKit reports success on an incomplete encoded stream, the
            // decoded size cannot establish completeness. Let the owner choose
            // a new name while the original stays in place.
            g_autoptr(GFile) folder = g_file_get_parent(file);
            g_autofree char *name = g_file_get_basename(file);
            g_autofree char *suggestion = g_strdup_printf("%s (1)", name);
            show_save_dialog(download, gtk_native_dialog_get_transient_for(dialog), suggestion, folder, TRUE);
            g_object_unref(download);
            g_object_unref(dialog);
            return;
        }
        if (save->replacing && !S_ISREG(original.st_mode) && !S_ISLNK(original.st_mode))
            g_clear_pointer(&path, g_free);
    }
    if (path) {
        g_autofree char *dir = g_path_get_dirname(path);
        save->staged = g_build_filename(dir, ".solstone-download-XXXXXX", NULL);
        int fd = g_mkstemp_full(save->staged, O_RDWR | O_CLOEXEC, 0600);
        if (fd >= 0) {
            close(fd);
            save->destination = g_steal_pointer(&path);
            webkit_download_set_allow_overwrite(download, TRUE);
            webkit_download_set_destination(download, save->staged);
        } else
            g_clear_pointer(&save->staged, g_free);
    }
    if (!save->destination)
        webkit_download_cancel(download);
    g_object_unref(download);
    g_object_unref(dialog);
}

static void show_save_dialog(WebKitDownload *download, GtkWindow *parent, const char *name,
                             GFile *folder, gboolean needs_new_name)
{
    const char *title = needs_new_name ? g_getenv("SOLSTONE_JOURNAL_NEW_NAME") : NULL;
    GtkFileChooserNative *dialog = gtk_file_chooser_native_new(title, parent, GTK_FILE_CHOOSER_ACTION_SAVE, NULL, NULL);
    if (folder)
        gtk_file_chooser_set_current_folder(GTK_FILE_CHOOSER(dialog), folder, NULL);
    gtk_file_chooser_set_current_name(GTK_FILE_CHOOSER(dialog), name);
    g_signal_connect(dialog, "response", G_CALLBACK(save_response), g_object_ref(download));
    gtk_native_dialog_show(GTK_NATIVE_DIALOG(dialog));
}

static gboolean choose_destination(WebKitDownload *download, const char *name, gpointer data)
{
    Journal *journal = data;
    const char *url = webkit_uri_request_get_uri(webkit_download_get_request(download));
    g_autoptr(GUri) uri = g_uri_parse(url, G_URI_FLAGS_NONE, NULL);
    g_autofree char *basename = journal_uri(journal, uri) &&
        g_strcmp0(g_uri_get_path(uri), "/app/import/api/journal-archive/export") == 0 ?
        g_strdup("journal-export.zip") : g_path_get_basename(name);
    show_save_dialog(download, journal->window, basename, NULL, FALSE);
    return TRUE;
}

static void download_started(WebKitNetworkSession *session, WebKitDownload *download, gpointer data)
{
    (void)session;
    Journal *journal = data;
    const char *url = webkit_uri_request_get_uri(webkit_download_get_request(download));
    g_autoptr(GUri) uri = g_uri_parse(url, G_URI_FLAGS_NONE, NULL);
    if (!journal_uri(journal, uri))
        webkit_download_cancel(download);
    else {
        PendingSave *save = g_new0(PendingSave, 1);
        g_object_set_data_full(G_OBJECT(download), "journal-save", save, free_save);
        g_signal_connect(download, "decide-destination", G_CALLBACK(choose_destination), journal);
        g_signal_connect(download, "failed", G_CALLBACK(download_failed), save);
        g_signal_connect(download, "finished", G_CALLBACK(download_finished), save);
    }
}

static gboolean key_pressed(GtkEventControllerKey *controller, guint key, guint code,
                            GdkModifierType modifiers, gpointer data)
{
    (void)controller; (void)code;
    WebKitWebView *view = data;
    modifiers &= gtk_accelerator_get_default_mod_mask();
    if (modifiers == GDK_ALT_MASK && key == GDK_KEY_Left && webkit_web_view_can_go_back(view))
        webkit_web_view_go_back(view);
    else if (modifiers == GDK_ALT_MASK && key == GDK_KEY_Right && webkit_web_view_can_go_forward(view))
        webkit_web_view_go_forward(view);
    else if ((modifiers == GDK_CONTROL_MASK && key == GDK_KEY_r) || (!modifiers && key == GDK_KEY_F5))
        webkit_web_view_reload(view);
    else
        return FALSE;
    return TRUE;
}

// WebKit may preconnect before a navigation decision. Reserve a non-listening
// loopback port for this view so every other destination fails locally, even
// on desktops with a system-wide proxy. GLib supports host:port exceptions.
static gboolean restrict_network(Journal *journal, WebKitNetworkSession *session)
{
    g_autoptr(GInetAddress) loopback = g_inet_address_new_loopback(G_SOCKET_FAMILY_IPV4);
    g_autoptr(GSocketAddress) bind_address = g_inet_socket_address_new(loopback, 0);
    journal->refusal_socket = g_socket_new(G_SOCKET_FAMILY_IPV4, G_SOCKET_TYPE_STREAM, G_SOCKET_PROTOCOL_TCP, NULL);
    if (!journal->refusal_socket || !g_socket_bind(journal->refusal_socket, bind_address, FALSE, NULL))
        return FALSE;
    g_autoptr(GSocketAddress) bound = g_socket_get_local_address(journal->refusal_socket, NULL);
    if (!bound)
        return FALSE;
    g_autofree char *proxy = g_strdup_printf("http://127.0.0.1:%u",
        g_inet_socket_address_get_port(G_INET_SOCKET_ADDRESS(bound)));
    g_autofree char *bridge = g_strdup_printf("127.0.0.1:%d", journal->port);
    const char *direct[] = {bridge, NULL};
    WebKitNetworkProxySettings *settings = webkit_network_proxy_settings_new(proxy, direct);
    webkit_network_session_set_proxy_settings(session, WEBKIT_NETWORK_PROXY_MODE_CUSTOM, settings);
    webkit_network_proxy_settings_free(settings);
    return TRUE;
}

static gboolean create_view(Journal *journal)
{
    g_autoptr(WebKitWebContext) context = webkit_web_context_new();
    const char *language = g_getenv("SOLSTONE_JOURNAL_LANGUAGE");
    if (language && *language) {
        const char *languages[] = { language, NULL };
        webkit_web_context_set_preferred_languages(context, languages);
    }
    g_autoptr(WebKitNetworkSession) session = webkit_network_session_new_ephemeral();
    if (!restrict_network(journal, session))
        return FALSE;
    g_autoptr(WebKitUserContentManager) manager = webkit_user_content_manager_new();
    g_autoptr(WebKitSettings) settings = webkit_settings_new();
    webkit_settings_set_user_agent_with_application_details(settings, HOST_PRODUCT, HOST_VERSION);
    webkit_settings_set_enable_webrtc(settings, FALSE);
    webkit_settings_set_enable_media_stream(settings, FALSE);
    g_autofree char *host = g_strdup_printf("http://127.0.0.1:%d", journal->port);
    g_autofree char *script = g_strdup_printf("if(location.origin==='%s'){%s}", host, HOST_INIT);
    WebKitUserScript *init = webkit_user_script_new(script, WEBKIT_USER_CONTENT_INJECT_TOP_FRAME,
        WEBKIT_USER_SCRIPT_INJECT_AT_DOCUMENT_START, NULL, NULL);
    webkit_user_content_manager_add_script(manager, init);
    webkit_user_script_unref(init);
    // WebKit applies this policy in addition to the journal's own HTTP policy.
    // Inline journal scripts/styles and local media blobs still work.
    g_autofree char *policy = g_strdup_printf(
        "default-src %1$s; connect-src %1$s; script-src %1$s 'unsafe-inline'; "
        "style-src %1$s 'unsafe-inline'; img-src %1$s data: blob:; font-src %1$s data:; "
        "media-src %1$s blob: data:; worker-src %1$s blob:; frame-src %1$s; "
        "form-action %1$s; base-uri %1$s; object-src 'none'", host);
    journal->view = WEBKIT_WEB_VIEW(g_object_new(WEBKIT_TYPE_WEB_VIEW,
        "web-context", context, "network-session", session, "user-content-manager", manager,
        "settings", settings, "default-content-security-policy", policy, NULL));
    journal->window = GTK_WINDOW(gtk_window_new());
    gtk_window_set_default_size(journal->window, 1000, 720);
    gtk_window_set_title(journal->window, "solstone");
    GtkEventController *keys = gtk_event_controller_key_new();
    gtk_event_controller_set_propagation_phase(keys, GTK_PHASE_CAPTURE);
    g_signal_connect(keys, "key-pressed", G_CALLBACK(key_pressed), journal->view);
    gtk_widget_add_controller(GTK_WIDGET(journal->window), keys);
    gtk_window_set_child(journal->window, GTK_WIDGET(journal->view));
    g_signal_connect(session, "download-started", G_CALLBACK(download_started), journal);
    g_signal_connect(journal->window, "close-request", G_CALLBACK(closed), journal);
    g_signal_connect(journal->view, "permission-request", G_CALLBACK(permission), journal);
    g_signal_connect(journal->view, "decide-policy", G_CALLBACK(decide), journal);
    g_signal_connect(journal->view, "load-changed", G_CALLBACK(loaded), journal);
    g_signal_connect(journal->view, "load-failed", G_CALLBACK(failed), journal);
    g_signal_connect(journal->view, "web-process-terminated", G_CALLBACK(terminated), journal);
    return TRUE;
}

// The GTK 4.6 compatibility build cannot attach WebKit's content tree to
// AT-SPI. Let the daemon open the same journal destination in the accessible
// system browser when a desktop requests a screen reader.
static gboolean screen_reader_requested(void)
{
    GSettingsSchemaSource *source = g_settings_schema_source_get_default();
    g_autoptr(GSettingsSchema) schema = source ? g_settings_schema_source_lookup(source,
        "org.gnome.desktop.a11y.applications", TRUE) : NULL;
    if (schema && g_settings_schema_has_key(schema, "screen-reader-enabled")) {
        g_autoptr(GSettings) settings = g_settings_new_full(schema, NULL, NULL);
        if (g_settings_get_boolean(settings, "screen-reader-enabled"))
            return TRUE;
    }
    g_autoptr(GDBusConnection) bus = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, NULL);
    if (!bus)
        return FALSE;
    g_autoptr(GVariant) reply = g_dbus_connection_call_sync(bus, "org.a11y.Bus", "/org/a11y/bus",
        "org.freedesktop.DBus.Properties", "Get",
        g_variant_new("(ss)", "org.a11y.Status", "ScreenReaderEnabled"), G_VARIANT_TYPE("(v)"),
        G_DBUS_CALL_FLAGS_NO_AUTO_START, 300, NULL, NULL);
    if (!reply)
        return FALSE;
    g_autoptr(GVariant) value = NULL;
    g_variant_get(reply, "(v)", &value);
    return g_variant_is_of_type(value, G_VARIANT_TYPE_BOOLEAN) && g_variant_get_boolean(value);
}

static gboolean incoming(GIOChannel *channel, GIOCondition condition, gpointer data)
{
    Journal *journal = data;
    g_autofree char *line = NULL;
    gsize length = 0;
    if ((condition & (G_IO_HUP | G_IO_ERR)) ||
        g_io_channel_read_line(channel, &line, &length, NULL, NULL) != G_IO_STATUS_NORMAL ||
        length > 4096 || !line) {
        g_main_loop_quit(journal->loop);
        return G_SOURCE_REMOVE;
    }
    g_strchomp(line);
    g_autoptr(GUri) uri = g_uri_parse(line, G_URI_FLAGS_NONE, NULL);
    if (!journal->view && uri)
        journal->port = g_uri_get_port(uri);
    if (!journal_uri(journal, uri) || journal->port <= 0 ||
        g_strcmp0(g_uri_get_path(uri), "/_bridge/bootstrap") != 0 ||
        !g_uri_get_query(uri) || g_uri_get_fragment(uri) || journal->pending) {
        g_main_loop_quit(journal->loop);
        return G_SOURCE_REMOVE;
    }
    if (screen_reader_requested()) {
        journal->pending = TRUE;
        answer(journal, FALSE);
        return G_SOURCE_REMOVE;
    }
    journal->pending = TRUE;
    if (!journal->view && !create_view(journal)) {
        answer(journal, FALSE);
        return G_SOURCE_REMOVE;
    }
    webkit_web_view_load_uri(journal->view, line);
    return G_SOURCE_CONTINUE;
}

int main(void)
{
    if (!gtk_init_check())
        return 1;
    Journal journal = { .loop = g_main_loop_new(NULL, FALSE) };
    g_autoptr(GIOChannel) input = g_io_channel_unix_new(STDIN_FILENO);
    g_io_add_watch(input, G_IO_IN | G_IO_HUP | G_IO_ERR, incoming, &journal);
    g_main_loop_run(journal.loop);
    g_clear_object(&journal.refusal_socket);
    g_main_loop_unref(journal.loop);
    return 0;
}
