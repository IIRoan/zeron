// The WPE backend delivers composited GPU buffers instead of repainting a GTK
// widget into Cairo. Kept in the helper: no WebKit or EGL library enters GPUI.
typedef struct { WPEView parent; } ZeronView;
typedef struct { WPEViewClass parent; } ZeronViewClass;
G_DEFINE_TYPE(ZeronView, zeron_view, WPE_TYPE_VIEW)

static gboolean zeron_copy_waiting(gpointer data) {
    Page *p = data;
    g_mutex_lock(&frame_mutex);
    gboolean pending = g_hash_table_contains(pending_frames, GUINT_TO_POINTER(p->id));
    g_mutex_unlock(&frame_mutex);
    if (p->visible && pending) return G_SOURCE_CONTINUE;
    WPEBuffer *buffer = p->waiting_buffer;
    p->waiting_buffer = NULL;
    p->waiting_source = 0;
    if (p->visible) {
        guint width = wpe_buffer_get_width(buffer), height = wpe_buffer_get_height(buffer);
        // A resize can finish while the compositor still owns the old buffers.
        if (width && height && width == p->width && height == p->height && width <= MAX_DIMENSION && height <= MAX_DIMENSION) {
            GError *failure = NULL;
            GBytes *bytes = wpe_buffer_import_to_pixels(buffer, &failure);
            gsize size = 0;
            const guchar *pixels = bytes ? g_bytes_get_data(bytes, &size) : NULL;
            // GBM's CPU mapping may have a different stride from the DMA-BUF
            // plane. WPE returns exactly height * mapStride bytes.
            gsize stride = WPE_IS_BUFFER_SHM(buffer)
                ? wpe_buffer_shm_get_stride(WPE_BUFFER_SHM(buffer)) : size / height;
            if (pixels && stride >= width * 4 && size >= (gsize)stride * height) {
                if (!p->reported_renderer && g_getenv("ZERON_BROWSER_GPU_STATS")) {
                    g_printerr("Zeron browser frame: %s %ux%u stride=%zu\n",
                        WPE_IS_BUFFER_DMA_BUF(buffer) ? "DMA-BUF" : "SHM", width, height, stride);
                    p->reported_renderer = TRUE;
                }
                guint length = 12 + width * height * 4;
                guchar *out = g_malloc(length);
                ((guint32 *)out)[0] = GUINT32_TO_LE(width);
                ((guint32 *)out)[1] = GUINT32_TO_LE(height);
                float scale = p->scale;
                guint32 bits;
                memcpy(&bits, &scale, sizeof bits);
                ((guint32 *)out)[2] = GUINT32_TO_LE(bits);
                // WPE's imported pixels use native ARGB32. GPUI expects BGRA
                // with straight alpha, matching the software helper's protocol.
                for (guint y = 0; y < height; y++) {
                    const guint32 *row = (const guint32 *)(pixels + y * stride);
                    guchar *dst = out + 12 + y * width * 4;
                    for (guint x = 0; x < width; x++) {
                        guint32 pixel = row[x];
                        guint alpha = pixel >> 24;
                        guint blue = pixel & 255, green = (pixel >> 8) & 255, red = (pixel >> 16) & 255;
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
                g_mutex_lock(&frame_mutex);
                g_hash_table_add(pending_frames, GUINT_TO_POINTER(p->id));
                g_mutex_unlock(&frame_mutex);
                send_owned_packet('F', p->id, out, length);
            } else if (failure) {
                g_free(p->error);
                p->error = g_strdup_printf("Could not read the browser GPU buffer: %s", failure->message);
                state(p);
            }
            g_clear_error(&failure);
        }
    }
    wpe_view_buffer_released(p->view, buffer);
    g_object_unref(buffer);
    return G_SOURCE_REMOVE;
}
static gboolean zeron_view_render(WPEView *view, WPEBuffer *buffer,
                                  const WPERectangle *damage, guint n_damage, GError **error) {
    Page *p = g_object_get_data(G_OBJECT(view), "zeron-page");
    wpe_view_buffer_rendered(view, buffer);
    if (!p || !p->visible) {
        wpe_view_buffer_released(view, buffer);
        return TRUE;
    }
    if (p->waiting_buffer) {
        wpe_view_buffer_released(view, p->waiting_buffer);
        g_clear_object(&p->waiting_buffer);
    }
    // Retain only the newest frame while output is full. In particular, a
    // static page's final update must survive a stalled reader.
    p->waiting_buffer = g_object_ref(buffer);
    if (!p->waiting_source && zeron_copy_waiting(p) == G_SOURCE_CONTINUE)
        p->waiting_source = g_timeout_add(16, zeron_copy_waiting, p);
    return TRUE;
}
static gboolean zeron_view_can_map(WPEView *view) { return TRUE; }
static void zeron_view_init(ZeronView *view) {}
static void zeron_view_class_init(ZeronViewClass *klass) {
    WPEViewClass *view = WPE_VIEW_CLASS(klass);
    view->render_buffer = zeron_view_render;
    view->can_be_mapped = zeron_view_can_map;
}

typedef struct {
    WPEDisplay parent;
    WPEDisplay *headless;
    WPEKeymap *keymap;
    WPEBufferFormats *formats;
} ZeronDisplay;
typedef struct { WPEDisplayClass parent; } ZeronDisplayClass;
G_DEFINE_TYPE(ZeronDisplay, zeron_display, WPE_TYPE_DISPLAY)

static gboolean zeron_display_connect(WPEDisplay *display, GError **error) {
    ZeronDisplay *self = (ZeronDisplay *)display;
    self->headless = wpe_display_headless_new();
    return wpe_display_connect(self->headless, error);
}
static WPEView *zeron_display_view(WPEDisplay *display) {
    return g_object_new(zeron_view_get_type(), "display", display, NULL);
}
static WPEToplevel *zeron_display_toplevel(WPEDisplay *display, guint max_views) {
    return g_object_new(WPE_TYPE_TOPLEVEL_HEADLESS, "display", display, NULL);
}
static gpointer zeron_display_egl(WPEDisplay *display, GError **error) {
    return wpe_display_get_egl_display(((ZeronDisplay *)display)->headless, error);
}
static WPEDRMDevice *zeron_display_drm(WPEDisplay *display) {
    return wpe_display_get_drm_device(((ZeronDisplay *)display)->headless);
}
static WPEKeymap *zeron_display_keymap(WPEDisplay *display) {
    ZeronDisplay *self = (ZeronDisplay *)display;
    if (!self->keymap) self->keymap = wpe_keymap_xkb_new();
    return self->keymap;
}
static WPEBufferFormats *zeron_display_formats(WPEDisplay *display) {
    ZeronDisplay *self = (ZeronDisplay *)display;
    if (!self->formats) {
        WPEDRMDevice *device = zeron_display_drm(display);
        if (!device) return NULL;
        WPEBufferFormatsBuilder *builder = wpe_buffer_formats_builder_new(device);
        // The current GPUI bridge reads pixels. Request GPU-rendered linear
        // ARGB so drivers can map it without a tiled-buffer conversion.
        wpe_buffer_formats_builder_append_group(builder, device, WPE_BUFFER_FORMAT_USAGE_MAPPING);
        wpe_buffer_formats_builder_append_format(builder, 0x34325241, 0); // DRM_FORMAT_ARGB8888, linear
        self->formats = wpe_buffer_formats_builder_end(builder);
    }
    // WPEDisplay adopts the vfunc result for its own cache.
    return g_object_ref(self->formats);
}
static void zeron_display_dispose(GObject *object) {
    ZeronDisplay *self = (ZeronDisplay *)object;
    g_clear_object(&self->headless);
    g_clear_object(&self->keymap);
    g_clear_object(&self->formats);
    G_OBJECT_CLASS(zeron_display_parent_class)->dispose(object);
}
static void zeron_display_init(ZeronDisplay *display) {}
static void zeron_display_class_init(ZeronDisplayClass *klass) {
    G_OBJECT_CLASS(klass)->dispose = zeron_display_dispose;
    WPEDisplayClass *display = WPE_DISPLAY_CLASS(klass);
    display->connect = zeron_display_connect;
    display->create_view = zeron_display_view;
    display->create_toplevel = zeron_display_toplevel;
    display->get_egl_display = zeron_display_egl;
    display->get_drm_device = zeron_display_drm;
    display->get_keymap = zeron_display_keymap;
    display->get_preferred_buffer_formats = zeron_display_formats;
}

static gboolean init_wpe(void) {
    display = g_object_new(zeron_display_get_type(), NULL);
    GError *error = NULL;
    if (!wpe_display_connect(display, &error)) {
        g_printerr("WPE display could not start: %s\n", error ? error->message : "unknown error");
        g_clear_error(&error);
        return FALSE;
    }
    WPEDRMDevice *device = wpe_display_get_drm_device(display);
    wpe_display_set_available_input_devices(display,
        WPE_AVAILABLE_INPUT_DEVICE_MOUSE | WPE_AVAILABLE_INPUT_DEVICE_KEYBOARD);
    if (device && wpe_drm_device_get_render_node(device)) {
        if (!wpe_display_get_egl_display(display, &error)) {
            g_printerr("WPE graphics could not start: %s\n", error ? error->message : "no EGL display");
            g_clear_error(&error);
            return FALSE;
        }
        g_printerr("Zeron browser: WPE hardware rendering on %s\n", wpe_drm_device_get_render_node(device));
    } else {
        g_printerr("Zeron browser: WPE software rendering (no usable DRM render node)\n");
    }
    return TRUE;
}

static void wpe_input(Page *p, JsonObject *o, const char *command) {
    guint mods = number(o, "mods");
    WPEModifiers modifiers = ((mods & 4) ? WPE_MODIFIER_KEYBOARD_CONTROL : 0)
        | ((mods & 1) ? WPE_MODIFIER_KEYBOARD_SHIFT : 0)
        | ((mods & 8) ? WPE_MODIFIER_KEYBOARD_ALT : 0)
        | ((mods & (1 << 26)) ? WPE_MODIFIER_KEYBOARD_META : 0)
        | (mods & (31 << 8));
    guint32 time = (guint32)(g_get_monotonic_time() / 1000);
    double x = number(o, "x") * p->scale, y = number(o, "y") * p->scale;
    WPEEvent *event;
    if (!strcmp(command, "key_down") || !strcmp(command, "key_up")) {
        guint keyval = xkb_keysym_from_name(string(o, "key"), XKB_KEYSYM_NO_FLAGS);
        if (!keyval) {
            gunichar value = g_utf8_get_char_validated(string(o, "key"), -1);
            if (value != (gunichar)-1 && value != (gunichar)-2) keyval = xkb_utf32_to_keysym(value);
        }
        WPEKeymapEntry *entries = NULL;
        guint count = 0, code = 0;
        WPEKeymap *keymap = wpe_display_get_keymap(display);
        if (keymap && wpe_keymap_get_entries_for_keyval(keymap, keyval, &entries, &count) && count)
            code = entries[0].keycode;
        g_free(entries);
        event = wpe_event_keyboard_new(!strcmp(command, "key_down") ? WPE_EVENT_KEYBOARD_KEY_DOWN : WPE_EVENT_KEYBOARD_KEY_UP,
            p->view, WPE_INPUT_SOURCE_KEYBOARD, time, modifiers, code, keyval);
    } else if (!strcmp(command, "scroll")) {
        event = wpe_event_scroll_new(p->view, WPE_INPUT_SOURCE_MOUSE, time, modifiers,
            -number(o, "dx") * 40, -number(o, "dy") * 40, TRUE, FALSE, x, y);
    } else if (!strcmp(command, "move")) {
        event = wpe_event_pointer_move_new(WPE_EVENT_POINTER_MOVE, p->view, WPE_INPUT_SOURCE_MOUSE,
            time, modifiers, x, y, x - p->pointer_x, y - p->pointer_y);
        p->pointer_x = x;
        p->pointer_y = y;
    } else {
        gboolean down = !strcmp(command, "down");
        guint button = number(o, "button");
        p->pointer_x = x;
        p->pointer_y = y;
        if (down) wpe_view_focus_in(p->view);
        guint count = down ? wpe_view_compute_press_count(p->view, x, y, button, time) : 0;
        event = wpe_event_pointer_button_new(down ? WPE_EVENT_POINTER_DOWN : WPE_EVENT_POINTER_UP,
            p->view, WPE_INPUT_SOURCE_MOUSE, time, modifiers, button, x, y, count);
    }
    wpe_view_event(p->view, event);
    wpe_event_unref(event);
    if (!strcmp(command, "scroll")) {
        // GPUI supplies scroll deltas without a native gesture-end event.
        // End the synthetic gesture so WebKit can settle its compositor.
        event = wpe_event_scroll_new(p->view, WPE_INPUT_SOURCE_MOUSE, time, modifiers,
                                    0, 0, TRUE, TRUE, x, y);
        wpe_view_event(p->view, event);
        wpe_event_unref(event);
    }
}
