// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#include <gio/gunixfdlist.h>

typedef struct {
    Journal *journal;
    WebKitScriptMessageReply *reply;
    JSCContext *context;
    char *id;
    char *options;
    GPtrArray *unreadable;
    gboolean intake;
    gboolean auth;
    int timeout_ms;
    const char *auth_fallback_type;
    const char *auth_fallback_outcome;
    const char *auth_fallback_reason;
    guint64 document_generation;
} ImportCall;

static char *string_field(JSCValue *object, const char *key)
{
    g_autoptr(JSCValue) value = jsc_value_object_get_property(object, key);
    return jsc_value_is_string(value) ? jsc_value_to_string(value) : NULL;
}

static void finish_import(ImportCall *call, const char *payload)
{
    g_autoptr(JSCValue) value = jsc_value_new_from_json(call->context, payload);
    if (call->intake) call->journal->import_busy = FALSE;
    if (!value || !jsc_value_is_object(value))
        webkit_script_message_reply_return_error_message(call->reply, "import unavailable");
    else {
        if (call->intake) {
            g_autoptr(JSCValue) id = jsc_value_new_string(call->context, call->id);
            g_autoptr(JSCValue) type = jsc_value_new_string(call->context, IMPORT_REPLY);
            jsc_value_object_set_property(value, "id", id);
            jsc_value_object_set_property(value, "type", type);
            if (call->unreadable) {
                g_autoptr(JSCValue) refused = jsc_value_object_get_property(value, "refused");
                if (!jsc_value_is_array(refused)) {
                    g_clear_object(&refused);
                    refused = jsc_value_new_array(call->context, G_TYPE_NONE);
                }
                g_autoptr(JSCValue) length = jsc_value_object_get_property(refused, "length");
                guint offset = jsc_value_to_int32(length);
                for (guint i = 0; i < call->unreadable->len; i++) {
                    g_autoptr(JSCValue) item = jsc_value_new_object(call->context, NULL, NULL);
                    g_autoptr(JSCValue) name = jsc_value_new_string(call->context, call->unreadable->pdata[i]);
                    jsc_value_object_set_property(item, "name", name);
                    jsc_value_object_set_property_at_index(refused, offset + i, item);
                }
                jsc_value_object_set_property(value, "refused", refused);
            }
        }
        g_autofree char *json = jsc_value_to_json(value, 0);
        g_autoptr(JSCValue) result = jsc_value_new_string(call->context, json);
        webkit_script_message_reply_return_value(call->reply, result);
    }
    webkit_script_message_reply_unref(call->reply);
    g_object_unref(call->context);
    g_free(call->id);
    g_free(call->options);
    g_clear_pointer(&call->unreadable, g_ptr_array_unref);
    g_free(call);
}

static void daemon_replied(GObject *source, GAsyncResult *result, gpointer data)
{
    ImportCall *call = data;
    g_autoptr(GVariant) answer = g_dbus_connection_call_with_unix_fd_list_finish(
        G_DBUS_CONNECTION(source), NULL, result, NULL);
    if (!answer && call->auth) {
        g_autoptr(JSCValue) obj = jsc_value_new_object(call->context, NULL, NULL);
        g_autoptr(JSCValue) type = jsc_value_new_string(call->context, call->auth_fallback_type);
        g_autoptr(JSCValue) id = jsc_value_new_string(call->context, call->id ? call->id : "");
        g_autoptr(JSCValue) outcome = jsc_value_new_string(call->context, call->auth_fallback_outcome);
        jsc_value_object_set_property(obj, "type", type);
        jsc_value_object_set_property(obj, "id", id);
        jsc_value_object_set_property(obj, "outcome", outcome);
        if (call->auth_fallback_reason) {
            g_autoptr(JSCValue) reason = jsc_value_new_string(call->context, call->auth_fallback_reason);
            jsc_value_object_set_property(obj, "reason", reason);
        }
        g_autofree char *encoded = jsc_value_to_json(obj, 0);
        finish_import(call, encoded ? encoded : "{}");
        return;
    }
    const char *json = call->intake ? "{\"outcome\":\"refused\"}" : "{\"state\":\"gone\"}";
    if (answer) g_variant_get(answer, "(&s)", &json);
    finish_import(call, json);
}

static void daemon_call(ImportCall *call, const char *method, GVariant *args, GUnixFDList *fds)
{
    int timeout = call->timeout_ms > 0 ? call->timeout_ms : (call->intake ? G_MAXINT : 5000);
    g_dbus_connection_call_with_unix_fd_list(call->journal->bus, "org.solpbc.solstone.Observer1",
        "/org/solpbc/solstone/Observer1", "org.solpbc.solstone.Observer1", method,
        args, G_VARIANT_TYPE("(s)"), G_DBUS_CALL_FLAGS_NO_AUTO_START,
        timeout, fds, NULL, daemon_replied, call);
}

static void cancel_auth_session(Journal *journal)
{
    if (!journal->bus) return;
    g_dbus_connection_call(journal->bus, "org.solpbc.solstone.Observer1",
        "/org/solpbc/solstone/Observer1", "org.solpbc.solstone.Observer1", "AuthCancel",
        g_variant_new("(s)", "{\"type\":\"auth.cancel\",\"id\":\"teardown\"}"),
        G_VARIANT_TYPE("(s)"), G_DBUS_CALL_FLAGS_NO_AUTO_START,
        2000, NULL, NULL, NULL);
}

static void reconcile_imports(Journal *journal)
{
    if (!journal->bus) return;
    g_dbus_connection_call(journal->bus, "org.solpbc.solstone.Observer1",
        "/org/solpbc/solstone/Observer1", "org.solpbc.solstone.Observer1", "ImportReconcile",
        NULL, G_VARIANT_TYPE("(s)"), G_DBUS_CALL_FLAGS_NO_AUTO_START,
        2000, NULL, NULL, NULL);
}

static void import_files(ImportCall *call, GListModel *files)
{
    g_autoptr(GUnixFDList) fds = g_unix_fd_list_new();
    GVariantBuilder entries;
    g_variant_builder_init(&entries, G_VARIANT_TYPE("a(sh)"));
    guint count = g_list_model_get_n_items(files);
    for (guint i = 0; i < count && count <= 64; i++) {
        g_autoptr(GFile) file = g_list_model_get_item(files, i);
        g_autofree char *path = g_file_get_path(file);
        g_autofree char *name = g_file_get_basename(file);
        int fd = path ? open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK) : -1;
        struct stat info;
        int index = name && g_utf8_validate(name, -1, NULL) && fd >= 0 && fstat(fd, &info) == 0 && S_ISREG(info.st_mode)
            ? g_unix_fd_list_append(fds, fd, NULL) : -1;
        if (fd >= 0) close(fd);
        if (index < 0) {
            if (!call->unreadable) call->unreadable = g_ptr_array_new_with_free_func(g_free);
            g_ptr_array_add(call->unreadable, g_utf8_make_valid(name ? name : "", -1));
            continue;
        }
        g_variant_builder_add(&entries, "(sh)", name, index);
    }
    if (!g_unix_fd_list_get_length(fds)) {
        g_variant_builder_clear(&entries);
        finish_import(call, "{\"outcome\":\"refused\"}");
        return;
    }
    daemon_call(call, "ImportPicked", g_variant_new("(a(sh)s)", &entries, call->options), fds);
}

static void picked_response(GtkNativeDialog *dialog, int response, gpointer data)
{
    ImportCall *call = data;
    if (response == GTK_RESPONSE_ACCEPT && call->document_generation == call->journal->document_generation) {
        g_autoptr(GListModel) files = gtk_file_chooser_get_files(GTK_FILE_CHOOSER(dialog));
        import_files(call, files);
    } else finish_import(call, "{\"outcome\":\"cancelled\"}");
    g_object_unref(dialog);
}

static gboolean import_message(WebKitUserContentManager *manager, JSCValue *packet,
                               WebKitScriptMessageReply *reply, gpointer data)
{
    (void)manager;
    Journal *journal = data;
    JSCContext *context = jsc_value_get_context(packet);
    g_autofree char *json = jsc_value_is_string(packet) ? jsc_value_to_string(packet) : NULL;
    g_autoptr(JSCValue) envelope = json && strlen(json) <= 65536 ? jsc_value_new_from_json(context, json) : NULL;
    g_autofree char *nonce = envelope && jsc_value_is_object(envelope) ? string_field(envelope, "nonce") : NULL;
    const char *current = webkit_web_view_get_uri(journal->view);
    g_autoptr(GUri) uri = current ? g_uri_parse(current, G_URI_FLAGS_NONE, NULL) : NULL;
    if (!journal_uri(journal, uri) || !nonce || g_strcmp0(journal->nonce, nonce) != 0) {
        webkit_script_message_reply_return_error_message(reply, "unavailable");
        return TRUE;
    }
    g_autoptr(JSCValue) msg = jsc_value_object_get_property(envelope, "message");
    if (!jsc_value_is_object(msg)) {
        webkit_script_message_reply_return_error_message(reply, "invalid message");
        return TRUE;
    }
    g_autofree char *type = string_field(msg, "type");
    g_autofree char *document_id = string_field(envelope, "documentId");
    if (!document_id || !g_uuid_string_is_valid(document_id)) {
        webkit_script_message_reply_return_error_message(reply, "unavailable");
        return TRUE;
    }
    if (!journal->document_id && g_strcmp0(type, "host.attach") == 0)
        journal->document_id = g_strdup(document_id);
    if (g_strcmp0(journal->document_id, document_id) != 0) {
        webkit_script_message_reply_return_error_message(reply, "unavailable");
        return TRUE;
    }
    ImportCall *call = g_new0(ImportCall, 1);
    call->journal = journal;
    call->document_generation = journal->document_generation;
    call->reply = webkit_script_message_reply_ref(reply);
    call->context = g_object_ref(context);
    call->id = string_field(msg, "id");
    if (g_strcmp0(type, "host.attach") == 0) {
        finish_import(call, "{}");
        return TRUE;
    }
    if (g_strcmp0(type, "host.ready") == 0) {
        journal->import_ready = TRUE;
        finish_import(call, "{}");
        return TRUE;
    }
    if (g_strcmp0(type, "host.status") == 0) {
        g_autofree char *item = string_field(msg, "item");
        if (!item || strlen(item) != 32) finish_import(call, "{\"state\":\"gone\"}");
        else daemon_call(call, "ImportItemStatus", g_variant_new("(s)", item), NULL);
        return TRUE;
    }
    if (g_strcmp0(type, AUTH_PREPARE) == 0 || g_strcmp0(type, AUTH_AUTHORIZE) == 0
        || g_strcmp0(type, AUTH_CANCEL) == 0) {
        if (!call->id || !*call->id || strlen(call->id) > 128) {
            finish_import(call, "{\"type\":\"unsupported\",\"id\":\"\"}");
            return TRUE;
        }
        g_autofree char *payload = jsc_value_to_json(msg, 0);
        if (!payload) {
            finish_import(call, "{\"type\":\"unsupported\",\"id\":\"\"}");
            return TRUE;
        }
        call->auth = TRUE;
        const char *method = NULL;
        if (g_strcmp0(type, AUTH_PREPARE) == 0) {
            method = "AuthPrepare";
            call->timeout_ms = 10000;
            call->auth_fallback_type = AUTH_PREPARED;
            call->auth_fallback_outcome = "unavailable";
            call->auth_fallback_reason = "host_unavailable";
        } else if (g_strcmp0(type, AUTH_AUTHORIZE) == 0) {
            method = "AuthAuthorize";
            call->timeout_ms = G_MAXINT;
            call->auth_fallback_type = AUTH_RESULT;
            call->auth_fallback_outcome = "error";
            call->auth_fallback_reason = "host_unavailable";
        } else {
            method = "AuthCancel";
            call->timeout_ms = 10000;
            call->auth_fallback_type = AUTH_RESULT;
            call->auth_fallback_outcome = "cancelled";
            call->auth_fallback_reason = NULL;
        }
        daemon_call(call, method, g_variant_new("(s)", payload), NULL);
        return TRUE;
    }
    if (g_strcmp0(type, IMPORT_PICK) != 0 || !call->id || strlen(call->id) > 128) {
        finish_import(call, "{\"type\":\"unsupported\"}");
        return TRUE;
    }
    // Only this request owns the busy flag.
    if (journal->import_busy) {
        g_autoptr(JSCValue) result = jsc_value_new_object(context, NULL, NULL);
        g_autoptr(JSCValue) id = jsc_value_new_string(context, call->id);
        g_autoptr(JSCValue) busy = jsc_value_new_string(context, "busy");
        g_autoptr(JSCValue) response_type = jsc_value_new_string(context, IMPORT_REPLY);
        jsc_value_object_set_property(result, "id", id);
        jsc_value_object_set_property(result, "type", response_type);
        jsc_value_object_set_property(result, "outcome", busy);
        g_autofree char *encoded = jsc_value_to_json(result, 0);
        finish_import(call, encoded);
        return TRUE;
    }
    call->intake = journal->import_busy = TRUE;
    g_autofree char *area = string_field(msg, "area");
    g_autoptr(JSCValue) ctx = jsc_value_object_get_property(msg, "context");
    g_autoptr(JSCValue) multiple = jsc_value_object_get_property(msg, "multiple");
    g_autoptr(JSCValue) accept = jsc_value_object_get_property(msg, "accept");
    if (!area || !*area || strlen(area) > 96 || !jsc_value_is_object(ctx) || jsc_value_is_array(ctx)
        || jsc_value_is_null(ctx) || !jsc_value_is_boolean(multiple) || !jsc_value_is_array(accept)) {
        finish_import(call, "{\"outcome\":\"refused\"}");
        return TRUE;
    }
    g_autofree char *context_json = jsc_value_to_json(ctx, 0);
    if (!context_json || strlen(context_json) > IMPORT_CONTEXT_LIMIT) {
        finish_import(call, "{\"outcome\":\"refused\"}");
        return TRUE;
    }
    g_autoptr(JSCValue) options = jsc_value_new_object(context, NULL, NULL);
    g_autoptr(JSCValue) value = jsc_value_new_string(context, area);
    jsc_value_object_set_property(options, "context", ctx);
    jsc_value_object_set_property(options, "area", value);
    call->options = jsc_value_to_json(options, 0);
    g_autofree char *drop = string_field(msg, "drop");
    if (drop) {
        if (journal->drop_files && !g_strcmp0(journal->drop_token, drop)) {
            g_autoptr(GListStore) selected = g_steal_pointer(&journal->drop_files);
            g_clear_pointer(&journal->drop_token, g_free);
            import_files(call, G_LIST_MODEL(selected));
        } else finish_import(call, "{\"outcome\":\"refused\"}");
        return TRUE;
    }
    GtkFileChooserNative *picker = gtk_file_chooser_native_new(NULL, journal->window,
        GTK_FILE_CHOOSER_ACTION_OPEN, NULL, NULL);
    gtk_file_chooser_set_select_multiple(GTK_FILE_CHOOSER(picker), jsc_value_to_boolean(multiple));
    GtkFileFilter *filter = gtk_file_filter_new();
    g_autoptr(GString) filter_name = g_string_new(NULL);
    g_autoptr(JSCValue) length = jsc_value_object_get_property(accept, "length");
    int count = jsc_value_to_int32(length);
    for (int i = 0; i < count && i < 64; i++) {
        g_autoptr(JSCValue) entry = jsc_value_object_get_property_at_index(accept, i);
        g_autofree char *token = jsc_value_is_string(entry) ? jsc_value_to_string(entry) : NULL;
        if (!token || !*token || strlen(token) > 128) continue;
        if (*token == '.') {
            g_autofree char *pattern = g_strconcat("*", token, NULL);
            gtk_file_filter_add_pattern(filter, pattern);
        } else if (strchr(token, '/')) gtk_file_filter_add_mime_type(filter, token);
        else continue;
        if (filter_name->len) g_string_append(filter_name, ", ");
        g_string_append(filter_name, token);
    }
    if (filter_name->len) {
        gtk_file_filter_set_name(filter, filter_name->str);
        gtk_file_chooser_add_filter(GTK_FILE_CHOOSER(picker), filter);
    }
    g_object_unref(filter);
    GtkFileFilter *all = gtk_file_filter_new();
    gtk_file_filter_set_name(all, "*");
    gtk_file_filter_add_pattern(all, "*");
    gtk_file_chooser_add_filter(GTK_FILE_CHOOSER(picker), all);
    g_object_unref(all);
    g_signal_connect(picker, "response", G_CALLBACK(picked_response), call);
    gtk_native_dialog_show(GTK_NATIVE_DIALOG(picker));
    return TRUE;
}

static gboolean accept_drop(GtkDropTarget *target, GdkDrop *drop, gpointer data)
{
    (void)target;
    return ((Journal *)data)->import_ready &&
        gdk_content_formats_contain_gtype(gdk_drop_get_formats(drop), GDK_TYPE_FILE_LIST);
}

static gboolean native_drop(GtkDropTarget *target, const GValue *value, double x, double y, gpointer data)
{
    (void)target; (void)x; (void)y;
    Journal *journal = data;
    GdkFileList *list = g_value_get_boxed(value);
    if (!journal->import_ready || !list) return FALSE;
    g_clear_object(&journal->drop_files);
    g_clear_pointer(&journal->drop_token, g_free);
    journal->drop_files = g_list_store_new(G_TYPE_FILE);
    for (GSList *file = gdk_file_list_get_files(list); file; file = file->next)
        g_list_store_append(journal->drop_files, file->data);
    journal->drop_token = g_uuid_string_random();
    g_autofree char *js = g_strdup_printf(
        "window.dispatchEvent(new CustomEvent(window.solstoneImportContract.drop,{cancelable:true,detail:{token:'%s'}}))",
        journal->drop_token);
    webkit_web_view_evaluate_javascript(journal->view, js, -1, NULL, NULL, NULL, NULL, NULL);
    return TRUE;
}
