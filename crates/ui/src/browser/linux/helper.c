#define _GNU_SOURCE
// WebKitGTK runs in its own process. Only rendered pixels and explicit browser
// commands cross the pipe; GPUI owns all visible windows and input routing.
#include <errno.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/file.h>
#include <glib-unix.h>
#include <json-glib/json-glib.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#ifdef ZERON_WPE
#include <wpe/webkit.h>
#include <wpe/headless/wpe-headless.h>
#include <cairo.h>
#include <xkbcommon/xkbcommon.h>
#else
#include <webkit2/webkit2.h>
#endif

#define MAX_COMMAND (1024 * 1024)
#define MAX_DIMENSION 8192

typedef struct {
    guint id;
#ifdef ZERON_WPE
    WPEView *view;
    double pointer_x, pointer_y;
    gboolean reported_renderer;
    WPEBuffer *waiting_buffer;
    guint waiting_source;
#else
    GtkWidget *window;
#endif
    WebKitWebView *web;
    gboolean visible, dirty;
    gchar *error;
    guint width, height;
    double scale;
    WebKitOptionMenu *options;
    gchar *context_link;
} Page;
static GHashTable *pages;
static WebKitWebContext *context;
static GByteArray *input;
static int profile_lock = -1;
static gboolean shutting_down;
#ifdef ZERON_WPE
static WPEDisplay *display;
static WebKitNetworkSession *network_session;
static GMainLoop *main_loop;
#define browser_quit() g_main_loop_quit(main_loop)
#else
#define browser_quit() gtk_main_quit()
#endif

typedef struct {
    char kind;
    guint id, length;
    void *data;
} Packet;
static GAsyncQueue *output;
static GMutex frame_mutex;
static GHashTable *pending_frames;

static gboolean output_failed(gpointer unused) {
    browser_quit();
    return G_SOURCE_REMOVE;
}

static gboolean write_all(const void *data, size_t length) {
    const char *p = data;
    while (length) {
        ssize_t n = write(STDOUT_FILENO, p, length);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0) {
            g_main_context_invoke(NULL, output_failed, NULL);
            return FALSE;
        }
        p += n;
        length -= n;
    }
    return TRUE;
}
static gpointer write_packets(gpointer unused) {
    for (;;) {
        Packet *p = g_async_queue_pop(output);
        guint32 header[2] = {GUINT32_TO_LE(p->id), GUINT32_TO_LE(p->length)};
        gboolean ok = write_all(&p->kind, 1) && write_all(header, sizeof header) &&
                      write_all(p->data, p->length);
        if (p->kind == 'F') {
            g_mutex_lock(&frame_mutex);
            g_hash_table_remove(pending_frames, GUINT_TO_POINTER(p->id));
            g_mutex_unlock(&frame_mutex);
        }
        g_free(p->data);
        g_free(p);
        if (!ok)
            return NULL;
    }
}
static void send_owned_packet(char kind, guint id, void *data, guint length) {
    Packet *p = g_new(Packet, 1);
    *p = (Packet){kind, id, length, data};
    g_async_queue_push(output, p);
}
static void send_packet(char kind, guint id, const void *data, guint length) {
    if (!output) {
        guint32 header[2] = {GUINT32_TO_LE(id), GUINT32_TO_LE(length)};
        if (write_all(&kind, 1) && write_all(header, sizeof header))
            write_all(data, length);
        return;
    }
    send_owned_packet(kind, id, g_memdup2(data, length), length);
}
static void send_json(char kind, guint id, JsonBuilder *builder) {
    JsonGenerator *gen = json_generator_new();
    JsonNode *root = json_builder_get_root(builder);
    json_generator_set_root(gen, root);
    gsize length;
    gchar *data = json_generator_to_data(gen, &length);
    send_packet(kind, id, data, length);
    g_free(data);
    json_node_free(root);
    g_object_unref(gen);
    g_object_unref(builder);
}
static void member_string(JsonBuilder *b, const char *name, const char *value) {
    json_builder_set_member_name(b, name);
    if (value)
        json_builder_add_string_value(b, value);
    else
        json_builder_add_null_value(b);
}
static void member_bool(JsonBuilder *b, const char *name, gboolean value) {
    json_builder_set_member_name(b, name);
    json_builder_add_boolean_value(b, value);
}
// Relay the platform IME through GPUI while WebKit retains editor semantics.
typedef struct {
    WebKitInputMethodContext parent;
    guint id;
    gchar *preedit;
    guint cursor;
} BrowserIM;
typedef struct {
    WebKitInputMethodContextClass parent;
} BrowserIMClass;
G_DEFINE_TYPE(BrowserIM, browser_im, WEBKIT_TYPE_INPUT_METHOD_CONTEXT)
static void im_preedit(WebKitInputMethodContext *ctx, gchar **text, GList **underlines,
                       guint *cursor) {
    BrowserIM *im = (BrowserIM *)ctx;
    *text = g_strdup(im->preedit ?: "");
    *cursor = im->cursor;
    *underlines =
        im->preedit && *im->preedit
            ? g_list_append(NULL,
                            webkit_input_method_underline_new(0, g_utf8_strlen(im->preedit, -1)))
            : NULL;
}
#ifdef ZERON_WPE
static gboolean im_filter(WebKitInputMethodContext *ctx, gpointer event) { return FALSE; }
#else
static gboolean im_filter(WebKitInputMethodContext *ctx, GdkEventKey *event) { return FALSE; }
#endif
static void im_focus(WebKitInputMethodContext *ctx, gboolean focused) {
    JsonBuilder *b = json_builder_new();
    json_builder_begin_object(b);
    member_bool(b, "focused", focused);
    json_builder_end_object(b);
    send_json('I', ((BrowserIM *)ctx)->id, b);
}
static void im_focus_in(WebKitInputMethodContext *ctx) { im_focus(ctx, TRUE); }
static void im_focus_out(WebKitInputMethodContext *ctx) { im_focus(ctx, FALSE); }
static void im_cursor(WebKitInputMethodContext *ctx, int x, int y, int w, int h) {
    JsonBuilder *b = json_builder_new();
    json_builder_begin_object(b);
    json_builder_set_member_name(b, "caret");
    json_builder_begin_array(b);
    json_builder_add_int_value(b, x);
    json_builder_add_int_value(b, y);
    json_builder_add_int_value(b, w);
    json_builder_add_int_value(b, h);
    json_builder_end_array(b);
    json_builder_end_object(b);
    send_json('I', ((BrowserIM *)ctx)->id, b);
}
static void im_surrounding(WebKitInputMethodContext *ctx, const gchar *text, guint length,
                           guint cursor, guint selection) {
    // Password contents must never leave WebKit's editor process boundary.
    if (webkit_input_method_context_get_input_purpose(ctx) == WEBKIT_INPUT_PURPOSE_PASSWORD)
        return;
    gchar *copy = g_strndup(text, length);
    JsonBuilder *b = json_builder_new();
    json_builder_begin_object(b);
    member_string(b, "text", copy);
    member_bool(b, "focused", TRUE);
    g_free(copy);
    json_builder_set_member_name(b, "cursor");
    json_builder_add_int_value(b, cursor);
    json_builder_set_member_name(b, "selection");
    json_builder_add_int_value(b, selection);
    json_builder_end_object(b);
    send_json('I', ((BrowserIM *)ctx)->id, b);
}
static void im_finalize(GObject *object) {
    g_free(((BrowserIM *)object)->preedit);
    G_OBJECT_CLASS(browser_im_parent_class)->finalize(object);
}
static void browser_im_class_init(BrowserIMClass *klass) {
    G_OBJECT_CLASS(klass)->finalize = im_finalize;
    WebKitInputMethodContextClass *im = WEBKIT_INPUT_METHOD_CONTEXT_CLASS(klass);
    im->get_preedit = im_preedit;
    im->filter_key_event = im_filter;
    im->notify_focus_in = im_focus_in;
    im->notify_focus_out = im_focus_out;
    im->notify_cursor_area = im_cursor;
    im->notify_surrounding = im_surrounding;
}
static void browser_im_init(BrowserIM *im) { im->preedit = g_strdup(""); }
static void state(Page *p) {
    JsonBuilder *b = json_builder_new();
    json_builder_begin_object(b);
    member_string(b, "url", webkit_web_view_get_uri(p->web));
    member_string(b, "title", webkit_web_view_get_title(p->web) ?: "");
    member_string(b, "error", p->error);
    member_bool(b, "loading", webkit_web_view_is_loading(p->web));
    member_bool(b, "can_back", webkit_web_view_can_go_back(p->web));
    member_bool(b, "can_forward", webkit_web_view_can_go_forward(p->web));
    json_builder_end_object(b);
    send_json('S', p->id, b);
}
static void changed(GObject *web, GParamSpec *spec, Page *p) { state(p); }
static void loaded(WebKitWebView *web, WebKitLoadEvent event, Page *p) {
    if (event == WEBKIT_LOAD_STARTED)
        g_clear_pointer(&p->error, g_free);
    state(p);
}
static gboolean failed(WebKitWebView *web, WebKitLoadEvent event, const char *uri, GError *error,
                       Page *p) {
    if (g_error_matches(error, WEBKIT_NETWORK_ERROR, WEBKIT_NETWORK_ERROR_CANCELLED))
        return TRUE;
    g_free(p->error);
    p->error = g_strdup(error->message);
    state(p);
    return TRUE;
}
static void terminated(WebKitWebView *web, WebKitWebProcessTerminationReason reason, Page *p) {
    g_free(p->error);
    p->error = g_strdup("The browser process stopped. Reload to try again.");
    state(p);
}
static gboolean allowed(const char *uri) {
    GUri *u = g_uri_parse(uri, G_URI_FLAGS_NONE, NULL);
    if (!u)
        return FALSE;
    const char *scheme = g_uri_get_scheme(u);
    gboolean ok = scheme && (!strcmp(scheme, "http") || !strcmp(scheme, "https")) &&
                  g_uri_get_host(u) && !g_uri_get_userinfo(u);
    g_uri_unref(u);
    return ok;
}
static gboolean policy(WebKitWebView *web, WebKitPolicyDecision *decision,
                       WebKitPolicyDecisionType type, Page *p) {
    if (type == WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION ||
        type == WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION) {
        WebKitNavigationAction *action = webkit_navigation_policy_decision_get_navigation_action(
            WEBKIT_NAVIGATION_POLICY_DECISION(decision));
        const char *uri = webkit_uri_request_get_uri(webkit_navigation_action_get_request(action));
        if (!allowed(uri)) {
            webkit_policy_decision_ignore(decision);
            return TRUE;
        }
        if (type == WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION) {
            if (webkit_navigation_action_is_user_gesture(action))
                send_packet('N', p->id, uri, strlen(uri));
            webkit_policy_decision_ignore(decision);
            return TRUE;
        }
    }
    return FALSE;
}
static gboolean permission(WebKitWebView *web, WebKitPermissionRequest *request, Page *p) {
    webkit_permission_request_deny(request);
    return TRUE;
}
static JsonBuilder *menu_builder(double x, double y) {
    JsonBuilder *b = json_builder_new();
    json_builder_begin_object(b);
    json_builder_set_member_name(b, "x");
    json_builder_add_double_value(b, x);
    json_builder_set_member_name(b, "y");
    json_builder_add_double_value(b, y);
    json_builder_set_member_name(b, "items");
    json_builder_begin_array(b);
    return b;
}
static void menu_item(JsonBuilder *b, const char *label, const char *action, gboolean enabled,
                      gboolean selected) {
    json_builder_begin_object(b);
    member_string(b, "label", label);
    member_string(b, "action", action);
    member_bool(b, "enabled", enabled);
    member_bool(b, "selected", selected);
    json_builder_end_object(b);
}
static void send_menu(Page *p, JsonBuilder *b) {
    json_builder_end_array(b);
    json_builder_end_object(b);
    send_json('M', p->id, b);
}
#ifdef ZERON_WPE
static gboolean context_menu(WebKitWebView *web, WebKitContextMenu *menu,
                             WebKitHitTestResult *hit, Page *p) {
    double x = p->pointer_x, y = p->pointer_y;
#else
static gboolean context_menu(WebKitWebView *web, WebKitContextMenu *menu, GdkEvent *event,
                             WebKitHitTestResult *hit, Page *p) {
    double x = 0, y = 0;
    gdk_event_get_coords(event, &x, &y);
#endif
    g_free(p->context_link);
    p->context_link = NULL;
    JsonBuilder *b = menu_builder(x / p->scale, y / p->scale);
    if (webkit_hit_test_result_context_is_link(hit)) {
        p->context_link = g_strdup(webkit_hit_test_result_get_link_uri(hit));
        menu_item(b, "Open link in new tab", "open-link", allowed(p->context_link), FALSE);
        menu_item(b, "Copy link address", "copy-link", TRUE, FALSE);
    }
    menu_item(b, "Copy", "copy",
              webkit_hit_test_result_context_is_selection(hit) ||
                  webkit_hit_test_result_context_is_editable(hit),
              FALSE);
    if (webkit_hit_test_result_context_is_editable(hit))
        menu_item(b, "Paste", "text", TRUE, FALSE);
    menu_item(b, "Select all", "select-all", TRUE, FALSE);
    menu_item(b, "Back", "back", webkit_web_view_can_go_back(web), FALSE);
    menu_item(b, "Forward", "forward", webkit_web_view_can_go_forward(web), FALSE);
    menu_item(b, "Reload", "reload", TRUE, FALSE);
    send_menu(p, b);
    return TRUE;
}
#ifdef ZERON_WPE
static gboolean option_menu(WebKitWebView *web, WebKitOptionMenu *menu,
                            WebKitRectangle *rect, Page *p) {
#else
static gboolean option_menu(WebKitWebView *web, WebKitOptionMenu *menu, GdkEvent *event,
                            GdkRectangle *rect, Page *p) {
#endif
    g_set_object(&p->options, menu);
    JsonBuilder *b = menu_builder(rect->x / p->scale, (rect->y + rect->height) / p->scale);
    for (guint i = 0; i < webkit_option_menu_get_n_items(menu); i++) {
        WebKitOptionMenuItem *item = webkit_option_menu_get_item(menu, i);
        gchar *action = g_strdup_printf("option:%u", i);
        menu_item(b, webkit_option_menu_item_get_label(item), action,
                  webkit_option_menu_item_is_enabled(item) &&
                      !webkit_option_menu_item_is_group_label(item),
                  webkit_option_menu_item_is_selected(item));
        g_free(action);
    }
    send_menu(p, b);
    return TRUE;
}
static void download(GObject *ctx, WebKitDownload *item, gpointer data) {
    webkit_download_cancel(item);
}
#ifndef ZERON_WPE
static gboolean damaged(GtkWidget *widget, GdkEvent *event, Page *p) {
    p->dirty = TRUE;
    return FALSE;
}
#else
static double number(JsonObject *o, const char *key);
static const char *string(JsonObject *o, const char *key);
#include "wpe_platform.h"
#endif
static void free_page(gpointer data) {
    Page *p = data;
    if (p->options) {
        webkit_option_menu_close(p->options);
        g_clear_object(&p->options);
    }
#ifdef ZERON_WPE
    g_object_set_data(G_OBJECT(p->view), "zeron-page", NULL);
    if (p->waiting_source) g_source_remove(p->waiting_source);
    if (p->waiting_buffer) {
        wpe_view_buffer_released(p->view, p->waiting_buffer);
        g_clear_object(&p->waiting_buffer);
    }
    wpe_view_unmap(p->view);
    g_object_unref(p->web);
#else
    gtk_widget_destroy(p->window);
#endif
    g_free(p->error);
    g_free(p->context_link);
    g_free(p);
}
static Page *new_page(guint id) {
    Page *p = g_new0(Page, 1);
    p->id = id;
    p->width = 800;
    p->height = 600;
    p->scale = 1;
    p->visible = TRUE;
#ifdef ZERON_WPE
    p->web = g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", display,
                         "web-context", context, "network-session", network_session, NULL);
    p->view = webkit_web_view_get_wpe_view(p->web);
    g_object_set_data(G_OBJECT(p->view), "zeron-page", p);
    WPEToplevel *toplevel = wpe_display_create_toplevel(display, 1);
    wpe_toplevel_resized(toplevel, p->width, p->height);
    wpe_view_set_toplevel(p->view, toplevel);
    g_object_unref(toplevel);
    wpe_view_resized(p->view, p->width, p->height);
    wpe_view_set_visible(p->view, TRUE);
    wpe_view_map(p->view);
#else
    p->window = gtk_offscreen_window_new();
    p->web = WEBKIT_WEB_VIEW(webkit_web_view_new_with_context(context));
#endif
    BrowserIM *im = g_object_new(browser_im_get_type(), NULL);
    im->id = id;
    webkit_web_view_set_input_method_context(p->web, WEBKIT_INPUT_METHOD_CONTEXT(im));
    g_object_unref(im);
    webkit_settings_set_enable_developer_extras(webkit_web_view_get_settings(p->web), FALSE);
#ifndef ZERON_WPE
    gtk_container_add(GTK_CONTAINER(p->window), GTK_WIDGET(p->web));
    gtk_window_set_default_size(GTK_WINDOW(p->window), p->width, p->height);
    g_signal_connect(p->window, "damage-event", G_CALLBACK(damaged), p);
#endif
    g_signal_connect(p->web, "notify::uri", G_CALLBACK(changed), p);
    g_signal_connect(p->web, "notify::title", G_CALLBACK(changed), p);
    g_signal_connect(p->web, "notify::is-loading", G_CALLBACK(changed), p);
    g_signal_connect(p->web, "load-changed", G_CALLBACK(loaded), p);
    g_signal_connect(p->web, "load-failed", G_CALLBACK(failed), p);
    g_signal_connect(p->web, "web-process-terminated", G_CALLBACK(terminated), p);
    g_signal_connect(p->web, "decide-policy", G_CALLBACK(policy), p);
    g_signal_connect(p->web, "permission-request", G_CALLBACK(permission), p);
    g_signal_connect(p->web, "context-menu", G_CALLBACK(context_menu), p);
    g_signal_connect(p->web, "show-option-menu", G_CALLBACK(option_menu), p);
#ifndef ZERON_WPE
    gtk_widget_show_all(p->window);
#endif
    g_hash_table_insert(pages, GUINT_TO_POINTER(id), p);
    return p;
}
#ifndef ZERON_WPE
static gboolean render_frames(gpointer unused) {
    GHashTableIter it;
    gpointer value;
    g_hash_table_iter_init(&it, pages);
    while (g_hash_table_iter_next(&it, NULL, &value)) {
        Page *p = value;
        if (!p->visible || !p->dirty)
            continue;
        // Never queue stale video or wait for the GPUI consumer on GTK's
        // event loop. Leave damage pending until the previous frame is sent.
        g_mutex_lock(&frame_mutex);
        gboolean pending = g_hash_table_contains(pending_frames, GUINT_TO_POINTER(p->id));
        g_mutex_unlock(&frame_mutex);
        if (pending)
            continue;
        p->dirty = FALSE;
        cairo_surface_t *surface = gtk_offscreen_window_get_surface(GTK_OFFSCREEN_WINDOW(p->window));
        if (!surface)
            continue;
        // Match GTK's get_pixbuf viewport in device coordinates. The backing
        // surface can be larger on a HiDPI display even with GDK_SCALE=1.
        cairo_rectangle_int_t viewport = {0, 0, p->width, p->height};
        cairo_surface_t *image = cairo_surface_map_to_image(surface, &viewport);
        cairo_format_t format = cairo_image_surface_get_format(image);
        guint w = cairo_image_surface_get_width(image), h = cairo_image_surface_get_height(image);
        if (cairo_surface_status(image) != CAIRO_STATUS_SUCCESS ||
            (format != CAIRO_FORMAT_ARGB32 && format != CAIRO_FORMAT_RGB24) ||
            w > MAX_DIMENSION || h > MAX_DIMENSION || w != p->width || h != p->height) {
            p->dirty = TRUE;
            cairo_surface_unmap_image(surface, image);
            continue;
        }
        cairo_surface_flush(image);
        guint stride = cairo_image_surface_get_stride(image);
        const guchar *src = cairo_image_surface_get_data(image);
        guint length = 12 + w * h * 4;
        guchar *out = g_malloc(length);
        ((guint32 *)out)[0] = GUINT32_TO_LE(w);
        ((guint32 *)out)[1] = GUINT32_TO_LE(h);
        float scale = p->scale;
        guint32 scale_bits;
        memcpy(&scale_bits, &scale, 4);
        ((guint32 *)out)[2] = GUINT32_TO_LE(scale_bits);
        // Cairo is already native-endian BGRA on our Linux targets. Avoid the
        // pixbuf conversion plus a second RGB -> BGRA pass on every frame.
        for (guint y = 0; y < h; y++) {
            const guint32 *row = (const guint32 *)(src + y * stride);
            guchar *dst = out + 12 + y * w * 4;
            for (guint x = 0; x < w; x++) {
                guint32 pixel = row[x];
                guint alpha = format == CAIRO_FORMAT_RGB24 ? 255 : pixel >> 24;
                guint blue = pixel & 255, green = (pixel >> 8) & 255, red = (pixel >> 16) & 255;
                // RenderImage accepts straight alpha; Cairo's source is premultiplied.
                if (alpha && alpha != 255) {
                    blue = MIN(255, (blue * 255 + alpha / 2) / alpha);
                    green = MIN(255, (green * 255 + alpha / 2) / alpha);
                    red = MIN(255, (red * 255 + alpha / 2) / alpha);
                }
                dst[x * 4] = blue;
                dst[x * 4 + 1] = green;
                dst[x * 4 + 2] = red;
                dst[x * 4 + 3] = alpha;
            }
        }
        cairo_surface_unmap_image(surface, image);
        g_mutex_lock(&frame_mutex);
        g_hash_table_add(pending_frames, GUINT_TO_POINTER(p->id));
        g_mutex_unlock(&frame_mutex);
        send_owned_packet('F', p->id, out, length);
    }
    return G_SOURCE_CONTINUE;
}
#endif
static double number(JsonObject *o, const char *key) {
    return json_object_has_member(o, key) ? json_object_get_double_member(o, key) : 0;
}
static const char *string(JsonObject *o, const char *key) {
    return json_object_has_member(o, key) ? json_object_get_string_member(o, key) : "";
}
static void input_event(Page *p, JsonObject *o, const char *command) {
#ifdef ZERON_WPE
    wpe_input(p, o, command);
#else
    GdkEventType type = !strcmp(command, "move")       ? GDK_MOTION_NOTIFY
                        : !strcmp(command, "down")     ? GDK_BUTTON_PRESS
                        : !strcmp(command, "up")       ? GDK_BUTTON_RELEASE
                        : !strcmp(command, "scroll")   ? GDK_SCROLL
                        : !strcmp(command, "key_down") ? GDK_KEY_PRESS
                                                       : GDK_KEY_RELEASE;
    GdkEvent *e = gdk_event_new(type);
    GdkWindow *window = gtk_widget_get_window(GTK_WIDGET(p->web));
    e->any.window = g_object_ref(window);
    e->any.send_event = TRUE;
    GdkSeat *seat = gdk_display_get_default_seat(gdk_window_get_display(window));
    gboolean key = type == GDK_KEY_PRESS || type == GDK_KEY_RELEASE;
    if (seat)
        gdk_event_set_device(e, key ? gdk_seat_get_keyboard(seat) : gdk_seat_get_pointer(seat));
    guint32 time = (guint32)(g_get_monotonic_time() / 1000);
    guint mods = number(o, "mods");
    double x = number(o, "x") * p->scale, y = number(o, "y") * p->scale;
    if (type == GDK_MOTION_NOTIFY) {
        e->motion.time = time;
        e->motion.x = x;
        e->motion.y = y;
        e->motion.state = mods;
    } else if (type == GDK_SCROLL) {
        e->scroll.time = time;
        e->scroll.x = x;
        e->scroll.y = y;
        e->scroll.state = mods;
        e->scroll.direction = GDK_SCROLL_SMOOTH;
        e->scroll.delta_x = number(o, "dx");
        e->scroll.delta_y = number(o, "dy");
    } else if (key) {
        e->key.time = time;
        e->key.state = mods;
        e->key.keyval = gdk_keyval_from_name(string(o, "key"));
        if (e->key.keyval == GDK_KEY_VoidSymbol) {
            gunichar u = g_utf8_get_char_validated(string(o, "key"), -1);
            if (u != (gunichar)-1 && u != (gunichar)-2)
                e->key.keyval = gdk_unicode_to_keyval(u);
        }
        e->key.string = g_strdup(string(o, "text"));
        e->key.length = strlen(e->key.string);
        GdkKeymapKey *keys = NULL;
        gint count = 0;
        if (gdk_keymap_get_entries_for_keyval(
                gdk_keymap_get_for_display(gdk_window_get_display(window)), e->key.keyval, &keys,
                &count) &&
            count) {
            e->key.hardware_keycode = keys[0].keycode;
            e->key.group = keys[0].group;
            g_free(keys);
        }
    } else {
        e->button.time = time;
        e->button.x = x;
        e->button.y = y;
        e->button.state = mods;
        e->button.button = number(o, "button");
        if (type == GDK_BUTTON_PRESS)
            gtk_widget_grab_focus(GTK_WIDGET(p->web));
    }
    gtk_widget_event(GTK_WIDGET(p->web), e);
    gdk_event_free(e);
#endif
}
static void copied(GObject *web, GAsyncResult *result, gpointer data) {
    GError *error = NULL;
    JSCValue *v = webkit_web_view_evaluate_javascript_finish(WEBKIT_WEB_VIEW(web), result, &error);
    if (v) {
        gchar *text = jsc_value_to_string(v);
        send_packet('C', GPOINTER_TO_UINT(data), text, strlen(text));
        g_free(text);
        g_object_unref(v);
    }
    g_clear_error(&error);
}
static void evaluated(GObject *web, GAsyncResult *result, gpointer data) {
    guint id = GPOINTER_TO_UINT(data);
    GError *error = NULL;
    JSCValue *v = webkit_web_view_evaluate_javascript_finish(WEBKIT_WEB_VIEW(web), result, &error);
    gchar *s = v ? jsc_value_to_json(v, 0) : g_strdup("null");
    send_packet('J', id, s ?: "null", strlen(s ?: "null"));
    g_free(s);
    g_clear_object(&v);
    g_clear_error(&error);
}

typedef struct { guint id; gchar *request_id; } Automation;
static void automation_reply(Automation *op, JsonNode *result, const char *error) {
    JsonBuilder *b = json_builder_new();
    json_builder_begin_object(b);
    member_string(b, "requestId", op->request_id);
    if (error) member_string(b, "error", error);
    else {
        json_builder_set_member_name(b, "result");
        if (result) json_builder_add_value(b, json_node_copy(result));
        else json_builder_add_null_value(b);
    }
    json_builder_end_object(b);
    send_json('A', op->id, b);
    g_free(op->request_id);
    g_free(op);
}
static void automation_evaluated(GObject *web, GAsyncResult *result, gpointer data) {
    GError *error = NULL;
    JSCValue *value = webkit_web_view_evaluate_javascript_finish(WEBKIT_WEB_VIEW(web), result, &error);
    gchar *text = value ? jsc_value_to_json(value, 0) : NULL;
    JsonParser *parser = json_parser_new();
    if (error) automation_reply(data, NULL, error->message);
    else if (text && strlen(text) > MAX_COMMAND) automation_reply(data, NULL, "browser_result_too_large: return a smaller result");
    else if (text && json_parser_load_from_data(parser, text, -1, NULL))
        automation_reply(data, json_parser_get_root(parser), NULL);
    else automation_reply(data, NULL, NULL);
    g_object_unref(parser);
    g_free(text);
    g_clear_object(&value);
    g_clear_error(&error);
}
static cairo_status_t screenshot_bytes(void *closure, const unsigned char *data, unsigned int length) {
    GByteArray *bytes = closure;
    if (bytes->len + length > 8 * 1024 * 1024) return CAIRO_STATUS_WRITE_ERROR;
    g_byte_array_append(bytes, data, length);
    return CAIRO_STATUS_SUCCESS;
}
static void automation_screenshot(GObject *web, GAsyncResult *result, gpointer data) {
    GError *error = NULL;
#ifdef ZERON_WPE
    WebKitImage *image = webkit_web_view_get_snapshot_finish(WEBKIT_WEB_VIEW(web), result, &error);
    GBytes *pixels = image ? webkit_image_as_bytes(image) : NULL;
    cairo_surface_t *surface = pixels ? cairo_image_surface_create_for_data(
        (guchar *)g_bytes_get_data(pixels, NULL), CAIRO_FORMAT_ARGB32,
        webkit_image_get_width(image), webkit_image_get_height(image), webkit_image_get_stride(image)) : NULL;
#else
    cairo_surface_t *surface = webkit_web_view_get_snapshot_finish(WEBKIT_WEB_VIEW(web), result, &error);
#endif
    if (!surface) {
        automation_reply(data, NULL, error ? error->message : "browser_screenshot_failed");
        g_clear_error(&error);
#ifdef ZERON_WPE
        g_clear_object(&image);
#endif
        return;
    }
    GByteArray *bytes = g_byte_array_new();
    if (cairo_surface_write_to_png_stream(surface, screenshot_bytes, bytes) != CAIRO_STATUS_SUCCESS) {
        automation_reply(data, NULL, "browser_screenshot_failed: image exceeds 8 MB or could not be encoded");
    } else {
        gchar *encoded = g_base64_encode(bytes->data, bytes->len);
        JsonBuilder *b = json_builder_new();
        json_builder_begin_object(b);
        member_string(b, "mimeType", "image/png");
        member_string(b, "data", encoded);
        json_builder_end_object(b);
        JsonNode *root = json_builder_get_root(b);
        automation_reply(data, root, NULL);
        json_node_free(root);
        g_object_unref(b);
        g_free(encoded);
    }
    g_byte_array_unref(bytes);
    cairo_surface_destroy(surface);
#ifdef ZERON_WPE
    g_object_unref(image);
#endif
}
static void shutdown_ready(GObject *manager, GAsyncResult *result, gpointer unused) {
    GError *error = NULL;
    GList *cookies = webkit_cookie_manager_get_cookies_finish(WEBKIT_COOKIE_MANAGER(manager), result, &error);
    g_list_free_full(cookies, (GDestroyNotify)soup_cookie_free);
    g_clear_error(&error);
    browser_quit();
}
static void begin_shutdown(void) {
    if (shutting_down)
        return;
    shutting_down = TRUE;
    g_hash_table_remove_all(pages);
    // Let WebKit process the pending cookie operations before leaving its loop.
    // get_cookies is available on older WebKitGTK 4.1 runtimes too (the
    // get_all_cookies API would require WebKitGTK 2.42).
#ifdef ZERON_WPE
    WebKitCookieManager *manager = webkit_network_session_get_cookie_manager(network_session);
#else
    WebKitCookieManager *manager = webkit_web_context_get_cookie_manager(context);
#endif
    webkit_cookie_manager_get_cookies(manager,
                                     "http://localhost/", NULL, shutdown_ready, NULL);
}

static void command(JsonObject *o) {
    guint id = number(o, "id");
    const char *cmd = string(o, "cmd");
    if (!strcmp(cmd, "shutdown")) {
        begin_shutdown();
        return;
    }
    if (shutting_down)
        return;
    Page *p = g_hash_table_lookup(pages, GUINT_TO_POINTER(id));
    if (!strcmp(cmd, "create")) {
        if (!p)
            new_page(id);
        return;
    }
    if (!p)
        return;
    if (!strcmp(cmd, "close")) {
        g_hash_table_remove(pages, GUINT_TO_POINTER(id));
        return;
    }
    if (!strcmp(cmd, "automate") || !strcmp(cmd, "screenshot")) {
        Automation *op = g_new0(Automation, 1);
        op->id = id;
        op->request_id = g_strdup(string(o, "requestId"));
        if (!strcmp(cmd, "screenshot"))
            webkit_web_view_get_snapshot(p->web, WEBKIT_SNAPSHOT_REGION_VISIBLE,
                                        WEBKIT_SNAPSHOT_OPTIONS_NONE, NULL, automation_screenshot, op);
        else webkit_web_view_evaluate_javascript(p->web, string(o, "script"), -1, NULL, NULL, NULL,
                                               automation_evaluated, op);
    } else if (!strcmp(cmd, "click")) {
        input_event(p, o, "move");
        input_event(p, o, "down");
        input_event(p, o, "up");
    } else if (!strcmp(cmd, "keypress")) {
        input_event(p, o, "key_down");
        input_event(p, o, "key_up");
    } else if (!strcmp(cmd, "load")) {
        const char *url = string(o, "url");
        if (allowed(url))
            webkit_web_view_load_uri(p->web, url);
    } else if (!strcmp(cmd, "dismiss-menu")) {
        if (p->options) {
            webkit_option_menu_close(p->options);
            g_clear_object(&p->options);
        }
    } else if (g_str_has_prefix(cmd, "option:")) {
        if (p->options) {
            guint index = g_ascii_strtoull(cmd + 7, NULL, 10);
            if (index < webkit_option_menu_get_n_items(p->options))
                webkit_option_menu_activate_item(p->options, index);
            webkit_option_menu_close(p->options);
            g_clear_object(&p->options);
        }
    } else if (!strcmp(cmd, "open-link")) {
        if (p->context_link && allowed(p->context_link))
            send_packet('N', p->id, p->context_link, strlen(p->context_link));
    } else if (!strcmp(cmd, "copy-link")) {
        if (p->context_link)
            send_packet('C', p->id, p->context_link, strlen(p->context_link));
    } else if (!strcmp(cmd, "select-all"))
        webkit_web_view_execute_editing_command(p->web, "SelectAll");
    else if (!strcmp(cmd, "reload"))
        webkit_web_view_reload(p->web);
    else if (!strcmp(cmd, "commit") || !strcmp(cmd, "preedit") || !strcmp(cmd, "unmark")) {
        BrowserIM *im = (BrowserIM *)webkit_web_view_get_input_method_context(p->web);
        if (!strcmp(cmd, "preedit")) {
            if (!*im->preedit)
                g_signal_emit_by_name(im, "preedit-started");
            g_free(im->preedit);
            im->preedit = g_strdup(string(o, "text"));
            im->cursor = g_utf8_strlen(im->preedit, -1);
            g_signal_emit_by_name(im, "preedit-changed");
        } else {
            if (!strcmp(cmd, "commit"))
                g_signal_emit_by_name(im, "committed", string(o, "text"));
            g_free(im->preedit);
            im->preedit = g_strdup("");
            im->cursor = 0;
            g_signal_emit_by_name(im, "preedit-changed");
            g_signal_emit_by_name(im, "preedit-finished");
        }
    } else if (!strcmp(cmd, "text"))
        webkit_web_view_execute_editing_command_with_argument(p->web, "InsertText",
                                                              string(o, "text"));
    else if (!strcmp(cmd, "copy") || !strcmp(cmd, "cut")) {
        const char *script =
            !strcmp(cmd, "cut")
                ? "(()=>{let e=document.activeElement;let s=e&&e.type==='password'?'':e&&typeof "
                  "e.selectionStart==='number'?e.value.slice(e.selectionStart,e.selectionEnd):"
                  "String(getSelection());if(s)document.execCommand('delete');return s})()"
                : "(()=>{let e=document.activeElement;return e&&e.type==='password'?'':e&&typeof "
                  "e.selectionStart==='number'?e.value.slice(e.selectionStart,e.selectionEnd):"
                  "String(getSelection())})()";
        webkit_web_view_evaluate_javascript(p->web, script, -1, NULL, NULL, NULL, copied,
                                            GUINT_TO_POINTER(id));
    } else if (!strcmp(cmd, "back"))
        webkit_web_view_go_back(p->web);
    else if (!strcmp(cmd, "forward"))
        webkit_web_view_go_forward(p->web);
    else if (!strcmp(cmd, "resize")) {
        guint w = CLAMP(number(o, "width"), 1, MAX_DIMENSION),
              h = CLAMP(number(o, "height"), 1, MAX_DIMENSION);
        double scale = CLAMP(number(o, "scale"), 0.5, 4);
        if (w != p->width || h != p->height || scale != p->scale) {
            p->width = w;
            p->height = h;
            p->scale = scale;
            webkit_web_view_set_zoom_level(p->web, scale);
#ifdef ZERON_WPE
            wpe_view_resized(p->view, w, h);
#else
            gtk_window_set_default_size(GTK_WINDOW(p->window), w, h);
            gtk_widget_set_size_request(GTK_WIDGET(p->web), w, h);
            gtk_widget_queue_resize(p->window);
#endif
            p->dirty = TRUE;
        }
    } else if (!strcmp(cmd, "visible")) {
        p->visible = number(o, "value") != 0;
#ifdef ZERON_WPE
        wpe_view_set_visible(p->view, p->visible);
#else
        if (p->visible) {
            gtk_widget_show(p->window);
            p->dirty = TRUE;
        } else
            gtk_widget_hide(p->window);
#endif
    } else if (!strcmp(cmd, "eval"))
        webkit_web_view_evaluate_javascript(p->web, string(o, "script"), -1, NULL, NULL, NULL,
                                            evaluated, GUINT_TO_POINTER(id));
    else
        input_event(p, o, cmd);
}
static gboolean read_commands(gint fd, GIOCondition condition, gpointer unused) {
    guint8 bytes[65536];
    ssize_t n = read(fd, bytes, sizeof bytes);
    if (n <= 0) {
        begin_shutdown();
        return G_SOURCE_REMOVE;
    }
    g_byte_array_append(input, bytes, n);
    while (input->len >= 4) {
        guint32 length;
        memcpy(&length, input->data, 4);
        length = GUINT32_FROM_LE(length);
        if (length > MAX_COMMAND) {
            browser_quit();
            return G_SOURCE_REMOVE;
        }
        if (input->len < 4 + length)
            break;
        JsonParser *parser = json_parser_new();
        if (json_parser_load_from_data(parser, (char *)input->data + 4, length, NULL)) {
            JsonNode *root = json_parser_get_root(parser);
            if (JSON_NODE_HOLDS_OBJECT(root))
                command(json_node_get_object(root));
        }
        g_object_unref(parser);
        g_byte_array_remove_range(input, 0, length + 4);
    }
    return G_SOURCE_CONTINUE;
}
static gboolean storage_directory(const gchar *path) {
    if (g_mkdir_with_parents(path, 0700) < 0)
        return FALSE;
    gchar *probe = g_build_filename(path, ".write-test-XXXXXX", NULL);
    int fd = g_mkstemp(probe);
    if (fd >= 0) {
        close(fd);
        unlink(probe);
    }
    g_free(probe);
    return fd >= 0;
}

#include "storage_compat.h"

static gboolean configure_storage(int argc, char **argv) {
    if (argc == 1) {
        // Explicitly unscoped contexts are used by fixtures/unresolved identity.
#ifdef ZERON_WPE
        context = webkit_web_context_new();
        network_session = webkit_network_session_new_ephemeral();
#else
        context = webkit_web_context_new_ephemeral();
#endif
        return TRUE;
    }
    if (argc != 3 || strcmp(argv[1], "--profile") || !g_path_is_absolute(argv[2])) {
        const char *error = "Browser profile requires an absolute storage directory";
        send_packet('E', 0, error, strlen(error));
        return FALSE;
    }
    if (!storage_directory(argv[2])) {
        const char *error = "Could not create or write the browser profile directory";
        send_packet('E', 0, error, strlen(error));
        return FALSE;
    }
    gchar *lock_path = g_build_filename(argv[2], "profile.lock", NULL);
    profile_lock = open(lock_path, O_RDWR | O_CREAT | O_CLOEXEC, 0600);
    g_free(lock_path);
    if (profile_lock < 0 || flock(profile_lock, LOCK_EX | LOCK_NB) < 0) {
        const char *error = "Browser profile is in use by another process or cannot be locked. Close the other Zeron instance and reopen this tab.";
        send_packet('E', 0, error, strlen(error));
        return FALSE;
    }
#ifdef ZERON_WPE
    if (!prepare_wpe_profile(argv[2])) return FALSE;
#else
    if (profile_uses_wpe(argv[2])) {
        const char *error = "This browser profile requires WPE WebKit 2.54 or newer. Install its runtime and use a WPE-enabled Zeron build; the GTK fallback cannot read its updated site storage.";
        send_packet('E', 0, error, strlen(error));
        return FALSE;
    }
#endif
    gchar *data = g_build_filename(argv[2], "data", NULL);
    gchar *cache = g_build_filename(argv[2], "cache", NULL);
    gchar *cookies = g_build_filename(argv[2], "cookies.sqlite", NULL);
    gboolean writable = storage_directory(argv[2]) && storage_directory(data) && storage_directory(cache);
    int fd = writable ? open(cookies, O_RDWR | O_CREAT | O_CLOEXEC, 0600) : -1;
    if (fd < 0) {
        gchar *error = g_strdup_printf("Could not open browser profile: %s", g_strerror(errno));
        send_packet('E', 0, error, strlen(error));
        g_free(error);
    } else {
        close(fd);
#ifdef ZERON_WPE
        context = webkit_web_context_new();
        network_session = webkit_network_session_new(data, cache);
        WebKitCookieManager *manager = webkit_network_session_get_cookie_manager(network_session);
        webkit_cookie_manager_set_persistent_storage(manager, cookies, WEBKIT_COOKIE_PERSISTENT_STORAGE_SQLITE);
#else
        WebKitWebsiteDataManager *manager = webkit_website_data_manager_new(
            "base-data-directory", data, "base-cache-directory", cache, NULL);
        webkit_cookie_manager_set_persistent_storage(
            webkit_website_data_manager_get_cookie_manager(manager), cookies,
            WEBKIT_COOKIE_PERSISTENT_STORAGE_SQLITE);
        context = webkit_web_context_new_with_website_data_manager(manager);
        g_object_unref(manager);
#endif
    }
    g_free(data);
    g_free(cache);
    g_free(cookies);
    return context != NULL;
}

int main(int argc, char **argv) {
    signal(SIGPIPE, SIG_IGN);
    umask(0077);
#ifdef ZERON_WPE
    main_loop = g_main_loop_new(NULL, FALSE);
    if (!init_wpe()) return 1;
#else
    // Offscreen GTK surfaces need CPU-addressable frames, never native GL child windows.
    g_setenv("WEBKIT_DISABLE_DMABUF_RENDERER", "1", TRUE);
    g_setenv("GDK_SCALE", "1", TRUE);
    g_setenv("GDK_DPI_SCALE", "1", TRUE);
    if (!gtk_init_check(&argc, &argv)) {
        g_printerr("Could not connect WebKitGTK to the display.\n");
        return 1;
    }
#endif
    pages = g_hash_table_new_full(g_direct_hash, g_direct_equal, NULL, free_page);
    input = g_byte_array_new();
    if (!configure_storage(argc, argv))
        return 1;
    output = g_async_queue_new();
    pending_frames = g_hash_table_new(g_direct_hash, g_direct_equal);
    g_thread_unref(g_thread_new("browser-output", write_packets, NULL));
    send_packet('R', 0, "", 0);
#ifdef ZERON_WPE
    g_signal_connect(network_session, "download-started", G_CALLBACK(download), NULL);
#else
    g_signal_connect(context, "download-started", G_CALLBACK(download), NULL);
#endif
    g_unix_fd_add(STDIN_FILENO, G_IO_IN | G_IO_HUP | G_IO_ERR, read_commands, NULL);
#ifdef ZERON_WPE
    g_main_loop_run(main_loop);
#else
    g_timeout_add(16, render_frames, NULL);
    gtk_main();
#endif
    g_hash_table_destroy(pages);
    g_byte_array_unref(input);
    g_object_unref(context);
#ifdef ZERON_WPE
    g_object_unref(network_session);
    g_object_unref(display);
    g_main_loop_unref(main_loop);
#endif
    if (profile_lock >= 0)
        close(profile_lock);
    return 0;
}
