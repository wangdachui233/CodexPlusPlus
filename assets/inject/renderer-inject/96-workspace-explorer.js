  /* Mount qualification adapted from code-codex packages/explorer-ui/src/inject.ts
   * and adapters/codex-26.715.ts.
   * MIT License
   *
   * Copyright (c) 2026 Code-Codex contributors
   *
   * Permission is hereby granted, free of charge, to any person obtaining a copy
   * of this software and associated documentation files (the "Software"), to deal
   * in the Software without restriction, including without limitation the rights
   * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
   * copies of the Software, and to permit persons to whom the Software is
   * furnished to do so, subject to the following conditions:
   *
   * The above copyright notice and this permission notice shall be included in all
   * copies or substantial portions of the Software.
   *
   * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
   * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
   * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
   * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
   * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
   * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
   * SOFTWARE.
   */
  function codexPlusWorkspaceExplorerVisible(node) {
    for (let current = node; current; current = current.parentElement) {
      if (current.hidden || current.matches('[data-app-shell-active-page="false"], [inert], [aria-hidden="true"]')) return false;
      const style = getComputedStyle(current);
      if (style.display === "none" || style.visibility === "hidden") return false;
    }
    return true;
  }

  function codexPlusWorkspaceExplorerMount() {
    const selector = 'main:is(.main-surface, [data-app-shell-main-surface="default"])';
    const active = (root) => [...root.querySelectorAll(selector)].filter(codexPlusWorkspaceExplorerVisible);
    const mains = active(document);
    if (mains.length !== 1 || !document.querySelector('[data-app-shell-sidebar-trigger]')) return null;
    const main = mains[0];
    const outer = main.closest('[data-app-shell-workspace-row="true"]');
    if (outer) {
      if (active(outer).length !== 1 || outer.querySelectorAll('aside.app-shell-left-panel').length !== 1) return null;
      let row = outer;
      if (![...row.children].some((child) => child.matches('aside.app-shell-left-panel'))) {
        const containers = [...row.children].filter((child) => child.matches('[data-app-shell-workspace="true"]'));
        if (containers.length !== 1 || !containers[0].contains(main)) return null;
        row = containers[0];
      }
      const children = [...row.children];
      const rails = children.filter((child) => child.matches('aside.app-shell-left-panel'));
      const workspaces = children.filter((child) => child.contains(main));
      if (rails.length !== 1 || workspaces.length !== 1 || children.indexOf(rails[0]) >= children.indexOf(workspaces[0]) || !main.parentElement?.className.includes('MainContentClip')) return null;
      return { parent: row, before: workspaces[0], main, modern: true };
    }
    const parent = main.parentElement;
    for (const shell of [parent, parent?.parentElement]) {
      if (!shell) continue;
      const rails = [...shell.children].filter((child) => child.matches('aside.app-shell-left-panel'));
      if (rails.length === 1 && active(shell).length === 1) return { parent, before: main, main, modern: false };
    }
    return null;
  }

  function codexPlusWorkspaceExplorerSession(main) {
    const visible = (selector, root = document) => [...root.querySelectorAll(selector)].filter(codexPlusWorkspaceExplorerVisible);
    const markers = visible('[data-above-composer-conversation-id]', main);
    if (markers.length !== 1) return "";
    const id = normalizedCodexThreadUuid(markers[0].getAttribute('data-above-composer-conversation-id'));
    if (!id || currentThreadScrollSessionId() !== id) return "";
    const responses = visible('[data-response-annotation-conversation]', main);
    if (responses.some((node) => node.getAttribute('data-response-annotation-conversation') !== id)) return "";
    const rows = visible('[data-app-action-sidebar-thread-active="true"]');
    if (!rows.length || rows.some((row) => {
      const encoded = row.getAttribute('data-app-action-sidebar-thread-id');
      const host = row.getAttribute('data-app-action-sidebar-thread-host-id');
      const kind = row.getAttribute('data-app-action-sidebar-thread-kind');
      return (host && host !== 'local') || (kind && kind !== 'local') || (encoded !== id && encoded !== `local:${id}`);
    })) return "";
    return id;
  }

  function codexPlusWorkspaceExplorerState() {
    return {
      generation: 0, previewGeneration: 0, sessionId: "", rootToken: null, nodes: new Map(), directories: new Map(),
      closePreview() { this.previewGeneration++; },
      reset(sessionId) {
        this.generation++;
        this.closePreview();
        this.sessionId = sessionId;
        this.rootToken = null;
        this.nodes.clear();
        this.directories.clear();
      },
    };
  }

  function codexPlusWorkspaceExplorerExtension(ctx) {
    const MAX_NODES = 10000;
    const MAX_DEPTH = 128;
    const state = codexPlusWorkspaceExplorerState();
    let panel = null, tree = null, overlay = null, restoreFocus = null, disposed = false, collapsed = false;
    const element = (tag, className, text) => {
      const node = document.createElement(tag);
      if (className) node.className = className;
      if (text !== undefined) node.textContent = text;
      return node;
    };
    const button = (text, action) => {
      const node = element('button', 'codex-plus-explorer-button', text);
      node.type = 'button';
      node.addEventListener('click', action);
      return node;
    };
    const enabled = () => !disposed && codexPlusBackendSettings.enhancementsEnabled !== false && codexPlusBackendSettings.codexAppWorkspaceExplorerEnabled === true;
    const closePreview = (focus = true) => {
      state.closePreview();
      overlay?.remove();
      overlay = null;
      if (focus) {
        const target = restoreFocus?.isConnected ? restoreFocus : [...(tree?.querySelectorAll('button') || [])].find((node) => node.title === restoreFocus?.title);
        (target || panel?.querySelector('button'))?.focus();
      }
      restoreFocus = null;
    };
    const reset = (id) => { closePreview(false); state.reset(id); };
    const current = (generation, id) => enabled() && generation === state.generation && id === state.sessionId && panel?.isConnected && codexPlusWorkspaceExplorerSession(codexPlusWorkspaceExplorerMount()?.main || panel) === id;
    const request = async (route, body) => {
      if (typeof window.__codexSessionDeleteBridge !== 'function') throw new Error('本地文件服务不可用');
      if (state.rootToken !== null) body.rootToken = state.rootToken;
      else if (route !== '/workspace-explorer/list' || body.path !== '' || body.cursor !== 0) throw new Error('工作目录尚未绑定，请刷新文件栏。');
      const result = await window.__codexSessionDeleteBridge(route, body);
      if (!result || result.status === 'error' || result.status === 'failed' || result.error) throw new Error(`${result?.message || result?.error || '读取失败'}；请刷新文件栏后重试。`);
      return result;
    };
    const directory = (path) => {
      if (!state.directories.has(path)) state.directories.set(path, { items: [], loaded: false, loading: false, expanded: path === '', cursor: null, truncated: false, error: '' });
      return state.directories.get(path);
    };
    const preview = async (path, trigger) => {
      closePreview(false);
      restoreFocus = trigger;
      const generation = state.generation, id = state.sessionId, token = ++state.previewGeneration;
      overlay = element('div', 'codex-plus-explorer-overlay');
      overlay.setAttribute('data-codex-plus-ext', 'workspace-explorer');
      const dialog = element('section', 'codex-plus-explorer-preview');
      dialog.setAttribute('role', 'dialog');
      dialog.setAttribute('aria-modal', 'true');
      dialog.setAttribute('aria-label', `只读预览：${path}`);
      const header = element('header', 'codex-plus-explorer-header');
      header.append(element('strong', '', path));
      const close = button('关闭', () => closePreview());
      header.append(close);
      const content = element('div', 'codex-plus-explorer-text', '正在读取…');
      dialog.append(header, content);
      overlay.append(dialog);
      document.body.append(overlay);
      close.focus();
      try {
        const result = await request('/workspace-explorer/preview', { sessionId: id, path });
        if (!current(generation, id) || token !== state.previewGeneration || !overlay) return;
        content.replaceChildren();
        const lines = String(result.text ?? '').split('\n');
        const numbers = element('pre', 'codex-plus-explorer-line-numbers', lines.map((_, index) => index + 1).join('\n'));
        numbers.setAttribute('aria-hidden', 'true');
        content.append(numbers, element('pre', 'codex-plus-explorer-source', String(result.text ?? '')));
        if (result.truncated) dialog.append(element('p', 'codex-plus-explorer-notice', '仅显示前 64 KiB，内容已截断。'));
      } catch (error) {
        if (current(generation, id) && token === state.previewGeneration && overlay) {
          content.replaceChildren(element('p', '', `无法预览：${error.message}`), button('重试', () => preview(path, trigger)));
        }
      }
    };
    const load = async (path) => {
      const dir = directory(path);
      if (dir.loading || !state.sessionId || state.nodes.size >= MAX_NODES) return;
      const generation = state.generation, id = state.sessionId;
      dir.loading = true; dir.error = '';
      render();
      try {
        const result = await request('/workspace-explorer/list', { sessionId: id, path, cursor: dir.cursor ?? 0, limit: Math.min(200, MAX_NODES - state.nodes.size) });
        if (!current(generation, id)) return;
        if (typeof result.rootToken !== 'string' || !result.rootToken || (state.rootToken !== null && state.rootToken !== result.rootToken)) throw new Error('工作目录绑定已变化，请刷新文件栏。');
        state.rootToken = result.rootToken;
        for (const item of result.items || []) {
          if (state.nodes.size >= MAX_NODES) { dir.truncated = true; break; }
          if (!state.nodes.has(item.path)) { state.nodes.set(item.path, item); dir.items.push(item.path); }
        }
        dir.loaded = true;
        dir.cursor = Number.isInteger(result.nextCursor) && result.nextCursor > (dir.cursor ?? -1) ? result.nextCursor : null;
        dir.truncated = dir.truncated || !!result.truncated;
      } catch (error) {
        if (!current(generation, id)) return;
        dir.error = error.message || '读取失败';
      } finally {
        if (generation === state.generation && id === state.sessionId) { dir.loading = false; render(); }
      }
    };
    const render = () => {
      if (!tree) return;
      tree.replaceChildren();
      if (!state.sessionId) { tree.append(element('p', 'codex-plus-explorer-notice', '等待可确认的本地会话与工作目录。')); return; }
      const appendDirectory = (path, depth, container) => {
        const dir = directory(path);
        for (const key of dir.items) {
          const item = state.nodes.get(key);
          const row = button(`${item.kind === 'directory' ? (state.directories.get(key)?.expanded ? '▾ ' : '▸ ') : ''}${item.name}${item.kind === 'link' ? '（链接，不读取）' : item.kind === 'other' ? '（不支持）' : ''}`, (event) => {
            if (item.kind === 'file') { void preview(key, event.currentTarget); return; }
            if (item.kind !== 'directory' || depth >= MAX_DEPTH) return;
            const child = directory(key);
            child.expanded = !child.expanded;
            if (!child.loaded && !child.loading && child.expanded) void load(key);
            render();
          });
          row.title = key;
          row.style.paddingLeft = `${8 + Math.min(depth, 20) * 12}px`;
          row.disabled = item.kind === 'link' || item.kind === 'other' || (item.kind === 'directory' && depth >= MAX_DEPTH);
          if (item.kind === 'directory') row.setAttribute('aria-expanded', String(!!state.directories.get(key)?.expanded));
          container.append(row);
          if (item.kind === 'directory' && state.directories.get(key)?.expanded && depth < MAX_DEPTH) appendDirectory(key, depth + 1, container);
        }
        if (dir.loading) container.append(element('p', 'codex-plus-explorer-notice', '正在读取…'));
        else if (dir.error) container.append(element('p', 'codex-plus-explorer-notice', `无法读取：${dir.error}`), button('重试', () => void load(path)));
        else if (dir.loaded && !dir.items.length) container.append(element('p', 'codex-plus-explorer-notice', '空目录'));
        if (dir.cursor !== null && !dir.loading && state.nodes.size < MAX_NODES) container.append(button('加载更多', () => void load(path)));
        if (dir.truncated) container.append(element('p', 'codex-plus-explorer-notice', '目录内容已截断。'));
      };
      appendDirectory('', 0, tree);
      if (state.nodes.size >= MAX_NODES) tree.append(element('p', 'codex-plus-explorer-notice', '已达到 10000 个缓存节点上限，请刷新后按需展开。'));
      if ([...state.nodes.values()].some((item) => item.kind === 'directory' && item.path.split('/').length >= MAX_DEPTH)) tree.append(element('p', 'codex-plus-explorer-notice', '目录展开深度上限为 128 层。'));
    };
    const detach = () => { panel?.remove(); panel = null; tree = null; reset(''); };
    const sync = () => {
      if (!enabled()) { if (panel || state.sessionId) detach(); return; }
      const mount = codexPlusWorkspaceExplorerMount();
      if (!mount) { if (panel || state.sessionId) detach(); return; }
      const id = codexPlusWorkspaceExplorerSession(mount.main);
      const changed = id !== state.sessionId || !panel;
      if (id !== state.sessionId) reset(id);
      if (!panel) {
        panel = element('aside', 'codex-plus-workspace-explorer');
        panel.setAttribute('data-codex-plus-ext', 'workspace-explorer');
        panel.setAttribute('aria-label', '工作目录（只读）');
        const header = element('header', 'codex-plus-explorer-header');
        const toggle = button(collapsed ? '展开文件栏' : '收起', () => {
          collapsed = !collapsed;
          panel.dataset.collapsed = String(collapsed);
          toggle.textContent = collapsed ? '展开文件栏' : '收起';
          toggle.setAttribute('aria-expanded', String(!collapsed));
        });
        toggle.setAttribute('aria-expanded', String(!collapsed));
        header.append(element('strong', '', '文件'), button('刷新', () => { reset(codexPlusWorkspaceExplorerSession(codexPlusWorkspaceExplorerMount()?.main || panel)); render(); if (state.sessionId) void load(''); }), toggle);
        tree = element('div', 'codex-plus-explorer-tree');
        panel.append(header, tree);
      }
      panel.dataset.collapsed = String(collapsed);
      panel.dataset.modern = String(mount.modern);
      if (panel.parentElement !== mount.parent || panel.nextElementSibling !== mount.before) mount.parent.insertBefore(panel, mount.before);
      if (id && !state.directories.has('')) void load('');
      else if (changed) render();
    };
    window.__codexPlusWorkspaceExplorerSync = sync;
    ctx.onScan(sync, { phase: 'light' });
    ctx.onScan(sync, { phase: 'deferred' });
    ctx.on('settings/loaded', sync);
    ctx.on('settings/changed', sync);
    ctx.listen(window, 'keydown', (event) => {
      if (!overlay) return;
      if (event.key === 'Escape') { event.preventDefault(); event.stopImmediatePropagation(); closePreview(); }
      else if (event.key === 'Tab') {
        const controls = [...overlay.querySelectorAll('button:not(:disabled)')];
        const index = controls.indexOf(document.activeElement);
        event.preventDefault(); event.stopImmediatePropagation();
        controls[(index + (event.shiftKey ? -1 : 1) + controls.length) % controls.length]?.focus();
      }
    }, true);
    ctx.effect(() => () => {
      disposed = true;
      detach();
      if (window.__codexPlusWorkspaceExplorerSync === sync) delete window.__codexPlusWorkspaceExplorerSync;
    });
    sync();
  }
