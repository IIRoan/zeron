// WebKit's 2022 API migrates LocalStorage/IndexedDB into unified origin
// storage. The old GTK API cannot read that format. Snapshot before the first
// WPE session and prevent a later missing runtime from opening a stale store.
static gboolean profile_uses_wpe(const char *profile) {
    gchar *marker = g_build_filename(profile, "wpe-storage-v1", NULL);
    gboolean result = g_file_test(marker, G_FILE_TEST_EXISTS);
    g_free(marker);
    return result;
}

#ifdef ZERON_WPE
static gboolean copy_profile_tree(const char *source, const char *destination, GError **error) {
    GFile *from = g_file_new_for_path(source);
    GFileInfo *info = g_file_query_info(from, G_FILE_ATTRIBUTE_STANDARD_TYPE,
        G_FILE_QUERY_INFO_NOFOLLOW_SYMLINKS, NULL, error);
    if (!info) { g_object_unref(from); return FALSE; }
    GFileType type = g_file_info_get_file_type(info);
    g_object_unref(info);
    gboolean ok = FALSE;
    if (type == G_FILE_TYPE_DIRECTORY) {
        if (g_mkdir_with_parents(destination, 0700) < 0) {
            g_set_error(error, G_FILE_ERROR, g_file_error_from_errno(errno), "Could not create profile snapshot: %s", g_strerror(errno));
        } else {
            GDir *directory = g_dir_open(source, 0, error);
            if (directory) {
                ok = TRUE;
                const gchar *name;
                while (ok && (name = g_dir_read_name(directory))) {
                    gchar *child = g_build_filename(source, name, NULL);
                    gchar *target = g_build_filename(destination, name, NULL);
                    ok = copy_profile_tree(child, target, error);
                    g_free(child);
                    g_free(target);
                }
                g_dir_close(directory);
            }
        }
    } else if (type == G_FILE_TYPE_REGULAR) {
        GFile *to = g_file_new_for_path(destination);
        ok = g_file_copy(from, to, G_FILE_COPY_NONE, NULL, NULL, NULL, error);
        g_object_unref(to);
    } else {
        g_set_error_literal(error, G_FILE_ERROR, G_FILE_ERROR_INVAL,
                            "Profile snapshot contains a symlink or special file");
    }
    g_object_unref(from);
    return ok;
}

static gboolean prepare_wpe_profile(const char *profile) {
    if (profile_uses_wpe(profile)) return TRUE;
    GError *error = NULL;
    gchar *data = g_build_filename(profile, "data", NULL);
    gchar *backup = g_build_filename(profile, "before-wpe-v1", NULL);
    gchar *complete = g_build_filename(backup, "snapshot-complete", NULL);
    if (g_file_test(backup, G_FILE_TEST_EXISTS) && !g_file_test(complete, G_FILE_TEST_IS_REGULAR))
        g_set_error_literal(&error, G_FILE_ERROR, G_FILE_ERROR_INVAL, "An incomplete profile snapshot already exists");
    if (!error && g_file_test(data, G_FILE_TEST_IS_DIR) && !g_file_test(backup, G_FILE_TEST_EXISTS)) {
        gchar *pending = g_build_filename(profile, ".before-wpe-XXXXXX", NULL);
        if (!g_mkdtemp_full(pending, 0700)) {
            g_set_error(&error, G_FILE_ERROR, g_file_error_from_errno(errno),
                        "Could not create profile snapshot: %s", g_strerror(errno));
        } else {
            const char *entries[] = {"data", "cookies.sqlite", "cookies.sqlite-wal", "cookies.sqlite-shm",
                "cache/CacheStorage", "cache/hsts-storage.sqlite", "cache/hsts-storage.sqlite-wal", "cache/hsts-storage.sqlite-shm", NULL};
            for (guint i = 0; entries[i] && !error; i++) {
                gchar *from = g_build_filename(profile, entries[i], NULL);
                gchar *to = g_build_filename(pending, entries[i], NULL);
                if (g_file_test(from, G_FILE_TEST_EXISTS)) {
                    gchar *parent = g_path_get_dirname(to);
                    if (g_mkdir_with_parents(parent, 0700) < 0)
                        g_set_error(&error, G_FILE_ERROR, g_file_error_from_errno(errno), "Could not create profile snapshot: %s", g_strerror(errno));
                    else copy_profile_tree(from, to, &error);
                    g_free(parent);
                }
                g_free(from);
                g_free(to);
            }
            if (!error) {
                gchar *marker = g_build_filename(pending, "snapshot-complete", NULL);
                g_file_set_contents(marker, "1\n", 2, &error);
                g_free(marker);
            }
            if (!error && rename(pending, backup) < 0)
                g_set_error(&error, G_FILE_ERROR, g_file_error_from_errno(errno), "Could not finish profile snapshot: %s", g_strerror(errno));
        }
        g_free(pending);
    }
    if (!error) {
        gchar *marker = g_build_filename(profile, "wpe-storage-v1", NULL);
        g_file_set_contents(marker, "1\n", 2, &error);
        g_free(marker);
    }
    g_free(data);
    g_free(backup);
    g_free(complete);
    if (!error) return TRUE;
    gchar *message = g_strdup_printf("Could not prepare the WPE browser profile: %s. The original storage was not opened.", error->message);
    send_packet('E', 0, message, strlen(message));
    g_free(message);
    g_error_free(error);
    return FALSE;
}
#endif
