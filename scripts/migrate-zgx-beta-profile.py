#!/usr/bin/env python3
"""Transfer regular Zeron's saved accounts and compatible settings to Linux beta.

Check-only unless --apply is supplied. Stop the beta first; regular Zeron
continues running. Originals are read only, and overwritten beta files are
backed up owner-only. Project registrations are restored separately by RPC.
"""
import argparse
import copy
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import tempfile
from urllib.parse import quote


def read(path, default=None):
    return json.loads(path.read_text()) if path.is_file() else copy.deepcopy(default)


def encode(value):
    return (json.dumps(value, indent=2, ensure_ascii=False) + '\n').encode()


def private_write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, name = tempfile.mkstemp(prefix='.' + path.name, dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as handle:
            os.fchmod(handle.fileno(), 0o600)
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(name, path)
    finally:
        if os.path.exists(name):
            os.unlink(name)


def identity(slot):
    return slot['harness'], slot['accountKey'], slot.get('storeKey')


def missing_environment_snapshots(source, target):
    """Follow-mode settings depend on these private, durable env copies.

    The main checkout need not still contain a followed file. Preserve any
    beta copy: it may include edits made since the original transfer.
    """
    root = source / 'worktree-environments/env-profiles'
    result = {}
    for path in root.rglob('*'):
        if path.is_symlink():
            raise SystemExit('Saved environment snapshots must be regular files.')
        if not path.is_file():
            continue
        destination = target / 'worktree-environments/env-profiles' / path.relative_to(root)
        if not destination.exists():
            result[destination] = path.read_bytes()
    return result


def slots(root):
    result = {}
    for path in root.glob('*/*.json'):
        value = read(path)
        if not isinstance(value, dict) or not all(k in value for k in ['id', 'harness', 'accountKey', 'credentials']):
            continue
        if path.parent.name != value['harness'] or path.stem != value['id']:
            raise SystemExit('Account slot filename or provider differs from its record.')
        result[identity(value)] = (path, value)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=Path.home() / '.zeron')
    parser.add_argument('--beta-profile', type=Path, default=Path.home() / '.local/share/zeron-beta/profile')
    parser.add_argument('--apply', action='store_true')
    args = parser.parse_args()
    source = args.source.resolve(strict=True)
    profile = args.beta_profile.resolve(strict=True)
    target = profile / 'engine'
    if source == target.resolve() or source in profile.parents or profile in source.parents:
        raise SystemExit('Source and beta profile must be separate.')
    ui = read(source / 'ui-settings.json', {})
    composer = read(source / 'composer-defaults.json', {})
    writes, deletes, converted = {}, [], []
    def save(path, value):
        writes[path] = encode(value)
    app = profile / 'data/zeron-beta/script-state'
    def state(module, key, value, script='app.luau'):
        save(app / quote(script, safe='._-') / (quote(f'{module}::{key}', safe='._-') + '.json'), value)
    def preference(source_key, key, convert=lambda x: x, module='screens/settings/prefs.luau'):
        if source_key in ui:
            state(module, key, convert(ui[source_key])); converted.append(source_key)

    # Preserve the original settings file too: engine banners read it directly,
    # and fields with no zgx counterpart are retained for a later port.
    save(target / 'ui-settings.json', {**read(target / 'ui-settings.json', {}), **ui})
    for name in ['harness-prefs.json', 'harness-update-prefs.json', 'composer-defaults.json', 'theme-library.json']:
        if (source / name).is_file():
            save(target / name, read(source / name))
    for old, new in {
        'composerSendBehavior': 'settings_send_with', 'escapeStopsActiveAgent': 'settings_escape_stops',
        'transcriptCompactMode': 'settings_compact_transcript', 'pullRequestOpenIn': 'settings_pull_requests',
        'filesAutosaveEnabled': 'settings_files_autosave', 'filesAutosaveDelayMs': 'settings_files_autosave_delay',
        'filesWordWrap': 'settings_files_word_wrap', 'filesShowAll': 'settings_files_show_all',
        'codeFontSize': 'settings_code_font_size', 'transcriptWidth': 'settings_conversation_width',
        'soundEnabled': 'settings_sounds', 'soundCompletionEnabled': 'settings_sound_done',
        'soundInputEnabled': 'settings_sound_request', 'soundAttentionEnabled': 'settings_sound_attention',
    }.items():
        preference(old, new)
    preference('composerSendBehavior', 'settings_send_with', lambda x: 'mod_enter' if x == 'modEnter' else x)
    preference('openWebLinksInZeron', 'settings_open_links', lambda x: 'zeron' if x else 'external')
    preference('appearance', 'appearance', module='screens/onboarding/state.luau')
    preference('uiFontSize', 'text_size', module='ds/look.luau')
    preference('accent', 'accent', lambda x: x.get('preset', 'default') if isinstance(x, dict) else 'default', 'ds/look.luau')
    preference('reduceMotion', 'motion_reduce', lambda x: x is True or x in ['on', 'always'], 'ds/motion.luau')
    preference('surface', 'frosted', lambda x: x != 'solid', 'ds/material.luau')
    preference('codeFencesFitContent', 'markdown_code_fit', module='ds/markdown.luau')
    for old, new in [('renderMath', 'math_render'), ('mathFont', 'math_face'), ('mathSize', 'math_size')]:
        preference(old, new, module='ds/math.luau')
    for old, new in [('appshotsEnabled','appshots_enabled'), ('appshotSoundEnabled','appshots_sound'), ('appshotDestination','appshots_destination')]:
        preference(old, new, module='screens/chat/appshots.luau')
    completion = {}
    for harness, item in ui.get('skillCompletionByHarness', {}).items():
        completion[harness] = {k: item[k] for k in ['dollar','separate'] if k in item}
    # The old global switch is the fallback for each agent's slash menu.
    for harness in ['codex','claude-code','cursor','devin','grok','pi','opencode','hermes','fx','muse','antigravity']:
        completion.setdefault(harness, {}).setdefault('separate', not ui.get('skillsInSlashMenu', False))
    state('screens/settings/prefs.luau', 'settings_skill_completion', completion)
    theme = ui.get('themeSelection', {})
    if theme:
        save(target / 'theme-choice.json', {**read(target / 'theme-choice.json', {}), **theme})
    # Keep layout preferences in the layout plugin's stable engine store.
    store_path = target / 'plugins/.storage/linux-workbench.json'
    store = read(store_path, {})
    for old, new in {
        'sidebarWidth':'shell_sidebar_width', 'sidebarCompact':'shell_sidebar_compact',
        'sidebarShowHarness':'shell_sidebar_show_harness', 'sidebarShowBranch':'shell_sidebar_show_branch',
        'sidebarShowPullRequest':'shell_sidebar_show_pr', 'sidebarShowProjectIcon':'shell_sidebar_show_project_icon',
        'sidebarShowProjectFilter':'shell_sidebar_show_project_filter', 'sidebarShowProjectLabel':'shell_sidebar_show_location',
        'githubStarBannerDismissed':'shell_star_banner_dismissed', 'rightPaneWidth':'shell_files_width',
        'terminalHeight':'fork.terminal_height', 'terminalOpen':'fork.terminal_dock',
    }.items():
        if old in ui:store[new] = ui[old];converted.append(old)
    store['shell_panel_width_chosen'] = True
    store['shell_sidebar_organize'] = ['project'] if ui.get('sidebarOrganization') == 'byProject' or ui.get('sidebarGrouped') else []
    store['fork.project_grouping_initialized'] = True
    store['shell_sidebar_sort'] = {'lastUpdated':'updated','createdAt':'created','alphabetical':'title'}.get(ui.get('sidebarSort'), 'updated')
    state('screens/shell/view.luau', 'shell_collapsed', ui.get('sidebarCollapsed', False))
    # Provider snapshots are keyed by account identity; retain the original
    # slot id/order while preserving a newer beta credential snapshot.
    originals = slots(source / 'agent-accounts')
    existing = slots(target / 'agent-accounts')
    merged = dict(existing)
    for key, (path, slot) in originals.items():
        chosen = copy.deepcopy(slot)
        if key in existing and existing[key][1].get('savedAt', 0) > slot.get('savedAt', 0):
            fresh = existing[key][1]
            for field in ['credentials','savedAt','claudeConfig']:
                if field in fresh:chosen[field] = fresh[field]
        merged[key] = (path, chosen)
    for key, (_, slot) in merged.items():
        save(target / 'agent-accounts' / slot['harness'] / f"{slot['id']}.json", slot)
        if key in existing and existing[key][1]['id'] != slot['id']:
            deletes.append(existing[key][0])
    old_usage = read(source / 'agent-accounts/usage-cache.json', {'entries':{}})
    usage = read(target / 'agent-accounts/usage-cache.json', {'entries':{}})
    save(target / 'agent-accounts/usage-cache.json', {'entries':{**old_usage.get('entries',{}), **usage.get('entries',{})}})
    # Services/configuration are copied; process ids, terminals and running
    # state belong to the source engine and must never be imported.
    services = read(target / 'profiles/local/project-terminals.json', {})
    for key, value in read(source / 'profiles/local/project-terminals.json', {}).items():
        item = copy.deepcopy(value);item['runs'] = [];item['environment'] = {}
        services[key] = item
    save(target / 'profiles/local/project-terminals.json', services)
    worktrees = read(target / 'worktree-environments/settings.json', {})
    for key, value in read(source / 'worktree-environments/settings.json', {}).items():
        item = copy.deepcopy(value);item['active'] = None
        for checkout in item.get('checkouts', {}).values():
            if checkout.get('state', {}).get('phase') == 'preparing':
                checkout['state'] = {'phase':'interrupted','step':'','error':None,'logAvailable':False}
        worktrees[key] = item
    save(target / 'worktree-environments/settings.json', worktrees)
    environment = missing_environment_snapshots(source, target)
    writes.update(environment)
    # Copy managed artwork to beta-owned paths and translate repository keys.
    icons = dict(store.get('project_icons', {}))
    for key, item in ui.get('projectIconsByProfile', {}).get('local', {}).items():
        path = Path(item['path'])
        if path.is_file():
            dest = target / 'project-icons' / path.name;writes[dest] = path.read_bytes()
            icons[key.removeprefix('repo:')] = str(dest)
    store['project_icons'] = icons
    background = ui.get('newThreadComposerBackground')
    if background and Path(background['path']).is_file():
        from PIL import Image
        path = Path(background['path']);dest = target / 'new-chat-backgrounds' / path.name
        writes[dest] = path.read_bytes()
        with Image.open(path) as image:w, h = image.size
        adjustment = background.get('adjustment', {})
        state('screens/shell/appearance.luau','shell_wallpaper','image')
        state('screens/shell/appearance.luau','shell_wallpaper_image',{'path':str(dest),'name':background.get('name',path.name),'w':w,'h':h,'x':adjustment.get('focalX',.5),'y':adjustment.get('focalY',.5),'zoom':adjustment.get('zoom',1)})
        state('screens/shell/appearance.luau','shell_wallpaper_effect',ui.get('newThreadBackgroundEffect','none'))
        state('screens/shell/appearance.luau','shell_wallpaper_plugin','')
    notes = read(source / 'release-notes-state.json', {})
    if 'show_after_updates' in notes:store['release_notes.show_after_updates'] = notes['show_after_updates']
    save(store_path, store)
    mapping = {'newSession':'shell.new_chat','newProject':'shell.new_project','archiveSession':'shell.archive',
               'openModelPicker':'shell.model_picker','toggleSidebar':'shell.toggle_sidebar','toggleChanges':'shell.toggle_files',
               'toggleFiles':'shell.toggle_files_tab','toggleTerminal':'shell.toggle_terminal','browserReload':'panel.reload_page',
               'nextSession':'shell.next_session','prevSession':'shell.previous_session'}
    def combo(s):return s.replace('mod-', 'cmd+').replace('ctrl-', 'ctrl+').replace('alt-', 'alt+').replace('shift-', 'shift+')
    keymap = ui.get('keymap', {});bound = {new:combo(keymap[old]) for old,new in mapping.items() if isinstance(keymap.get(old),str)}
    for index, key in enumerate(keymap.get('jumpSession', []), 1):bound[f'shell.session_{index}'] = combo(key)
    state('screens/shell/keys.luau','shell_keymap',bound)
    if keymap.get('captureAppshot'):state('screens/chat/appshots.luau','appshots_shortcut',combo(keymap['captureAppshot']))
    favorites = {f"{f['harness']}/{f['model']}":True for f in composer.get('favorites', [])}
    state('ds/model_picker.luau','model_favorites',favorites)
    # Git has a private copy of the standard diff components in its own VM.
    for key, old in [('ds.git.diff_split','diffSplit'),('ds.git.diff_wrap','diffWrap')]:
        if old in ui:
            state('ui/git.luau',key,ui[old],script='plugin:zeron-git')
            state('ds/git.luau',key,ui[old])
    default_path = app / 'app.luau' / (quote('screens/chat/composer.luau::chat_new_defaults',safe='._-')+'.json')
    defaults = read(default_path, {})
    harness = composer.get('harness');model = composer.get('modelByHarness',{}).get(harness,{})
    if harness and model.get('id'):
        defaults['model'] = f"{harness}/{model['id']}"
        defaults['effort'] = composer.get('reasoningByModel',{}).get(harness,{}).get(model['id'],composer.get('reasoning'))
        defaults['options'] = composer.get('modelOptionsByModel',{}).get(harness,{}).get(model['id'],{})
        state('screens/chat/composer.luau','chat_new_defaults',defaults)
    state('screens/shell/route.luau','shell_last_route','/settings/providers')
    changed = {p:b for p,b in writes.items() if not p.is_file() or p.read_bytes()!=b}
    counts = {}
    for _,slot in merged.values():counts[slot['harness']] = counts.get(slot['harness'],0)+1
    plan = {'applied':args.apply,'saved_account_counts':counts,'converted_preferences':len(set(converted)),
            'files_to_write':len(changed),'obsolete_duplicate_slots':len(deletes),'service_profiles':len(services),
            'worktree_projects':len(worktrees),'project_icons':len(icons),'original_profile_unchanged':True}
    plan['missing_environment_snapshots'] = len(environment)
    if not args.apply:
        print(json.dumps(plan,indent=2));return
    for proc in Path('/proc').iterdir():
        if not proc.name.isdecimal():continue
        try:
            if ('ZERON_DATA_DIR='+str(target)).encode() in (proc/'environ').read_bytes().split(b'\0'):
                raise SystemExit('Stop Zeron Beta before applying this transfer.')
        except (PermissionError,FileNotFoundError,ProcessLookupError):pass
    backup = profile.parent / 'migration-backups' / datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S.%fZ')
    backup.mkdir(parents=True,mode=0o700)
    journal = {'changed':[],'created':[],'deleted':[]}
    for path in sorted(set(changed) | set(deletes)):
        relative = path.relative_to(profile)
        if path.is_file():
            private_write(backup/relative,path.read_bytes());journal['changed'].append(str(relative))
        else:journal['created'].append(str(relative))
    private_write(backup/'journal.json',encode(journal))
    for path,data in changed.items():private_write(path,data)
    for path in deletes:
        if path not in writes and path.exists():path.unlink();journal['deleted'].append(str(path.relative_to(profile)))
    private_write(backup/'journal.json',encode(journal))
    plan['backup'] = str(backup)
    private_write(backup/'receipt.json',encode(plan))
    print(json.dumps(plan,indent=2))


if __name__ == '__main__':
    main()
