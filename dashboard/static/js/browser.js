// Nomad Network browser panel: address bar + discovered-node picker + iframe
// pointed at the `nomad://` custom scheme (registered in src-tauri).

var BROWSER_NODES_AUTO_REFRESH_MS = 5000;

var _browserNodesRaw = [];
var _browserNodesFilter = '';
var _browserNodesSearchTimer = null;
var _browserNodesAutoRefreshTimer = null;

// NomadNet convention (see markqvist/NomadNet's Browser.DEFAULT_PATH and
// reticulum-meshchat's defaultNodePagePath): nodes are always asked for
// /page/index.mu with no fallback to index.html — nomad-core (the server
// this app talks to) guarantees that path resolves to something, either the
// operator's real index.mu or an auto-generated listing page.
var NOMAD_DEFAULT_PAGE_PATH = '/page/index.mu';

function _browserDefaultUrlFor(identityHash) {
    return 'nomad://' + identityHash + NOMAD_DEFAULT_PAGE_PATH;
}

// Per-session history. `_browserHistory[_browserHistoryPos]` is the entry
// currently displayed; navigating somewhere new truncates anything ahead of
// it, exactly like a normal browser's forward stack.
var _browserHistory = [];
var _browserHistoryPos = -1;
// Set while a back/forward load is in flight so the frame's own href report
// is not mistaken for a new navigation.
var _browserSuppressHistoryPush = false;

function _browserUpdateNavButtons() {
    var back = document.getElementById('browser-back');
    var fwd = document.getElementById('browser-forward');
    if (back) back.disabled = _browserHistoryPos <= 0;
    if (fwd) fwd.disabled = _browserHistoryPos < 0 ||
        _browserHistoryPos >= _browserHistory.length - 1;
}

/// Records a URL as the current history entry. Consecutive duplicates are
/// collapsed so a reload does not stack up entries.
function _browserHistoryPush(url) {
    if (_browserHistory[_browserHistoryPos] === url) {
        _browserUpdateNavButtons();
        return;
    }
    _browserHistory = _browserHistory.slice(0, _browserHistoryPos + 1);
    _browserHistory.push(url);
    _browserHistoryPos = _browserHistory.length - 1;
    _browserUpdateNavButtons();
}

// Points the frame at `url`, abandoning whatever it was loading first.
//
// A nomad:// fetch can sit pending for the full request timeout (an
// unreachable or slow node), and assigning a new `src` on top of an
// in-flight custom-scheme load leaves the frame showing the stale pending
// request — so the browser looks wedged until the old one gives up. Blanking
// the frame first tears that load down, so a new navigation always starts
// immediately instead of queueing behind a dead one. The old request may
// still run to completion in the background, but nothing waits on it.
/// Shows or hides the in-flight indicator.
function _browserSetLoading(on) {
    var el = document.getElementById('browser-loading');
    if (el) el.classList.toggle('is-active', !!on);
}

/// Shows or hides the download-in-flight indicator. Separate element from
/// _browserSetLoading (navigation) so starting one doesn't hide the other —
/// a file link click and a page load can be in flight at the same time.
function _browserSetDownloading(on) {
    var el = document.getElementById('browser-download-loading');
    if (el) el.classList.toggle('is-active', !!on);
    if (on) _browserSetDownloadProgressText('Downloading…');
}

function _browserSetDownloadProgressText(text) {
    var el = document.getElementById('browser-download-loading-text');
    if (el) el.textContent = text;
}

// 'nomad_download_progress' (see api_nomad_file_download) fires for the file
// download currently in flight -- the panel only ever runs one at a time, so
// there's no id to key off. total_bytes is 0 until the reply's resource
// advertisement arrives; small/inline files never advertise one at all, so
// this may never fire for them (they finish before a listener would matter).
RS.listen('nomad_download_progress', function(data) {
    if (!data || !data.total_bytes) return;
    var percent = Math.min(99, Math.max(1, Math.round((data.bytes_received / data.total_bytes) * 100)));
    _browserSetDownloadProgressText(
        'Downloading… ' + percent + '% (' + prettySize(data.bytes_received) + ' of ' + prettySize(data.total_bytes) + ')'
    );
});

function _browserLoadIntoFrame(url) {
    var frame = document.getElementById('browser-frame');
    var input = document.getElementById('browser-address-input');
    if (frame) {
        if (frame.getAttribute('src')) frame.src = 'about:blank';
        frame.src = url;
        // Same moment the previous load is abandoned, so the indicator tracks
        // exactly the request the frame is actually waiting on.
        _browserSetLoading(true);
    }
    if (input) input.value = url;
}

function _browserNavigate(url) {
    if (!url || url.indexOf('nomad://') !== 0) return;
    _browserLoadIntoFrame(url);
    _browserHistoryPush(url);
}

// A file link inside the frame (see NOMAD_ADDRESS_SYNC_SCRIPT in src-tauri)
// posts 'nomad-download' instead of navigating. Fetches the bytes over IPC
// and hands them to the same blob-download path LXMF attachments use
// (RS.saveDownloadedFile), which is what actually reaches a save dialog /
// mobile share sheet — the nomad:// scheme's own responses never do.
function _browserDownloadFile(url) {
    _browserSetDownloading(true);
    RS.invoke('api_nomad_file_download', { url: url }).then(function(result) {
        var raw = atob(result.data_base64);
        var arr = new Uint8Array(raw.length);
        for (var i = 0; i < raw.length; i++) arr[i] = raw.charCodeAt(i);
        var blob = new Blob([arr], { type: result.mime || 'application/octet-stream' });
        return RS.saveDownloadedFile({
            url: URL.createObjectURL(blob),
            filename: result.filename || 'download',
            mime: result.mime || 'application/octet-stream'
        });
    }).then(function() {
        _browserSetDownloading(false);
        showToast('Saved', 'toast-green', 2200);
    }).catch(function(err) {
        _browserSetDownloading(false);
        showToast('Download failed: ' + ((err && err.message) || 'unknown error'), 'toast-red', 4000);
    });
}

/// Moves `delta` entries through history without pushing a new one.
///
/// The loaded page reports its own href back over postMessage, which would
/// otherwise be recorded as a *new* navigation and discard the forward stack.
/// Comparing URL strings is not enough — the frame reports its normalised
/// `location.href`, which need not match the stored entry byte for byte — so
/// the next report is suppressed explicitly.
function _browserGo(delta) {
    var target = _browserHistoryPos + delta;
    if (target < 0 || target >= _browserHistory.length) return;
    _browserHistoryPos = target;
    _browserSuppressHistoryPush = true;
    _browserLoadIntoFrame(_browserHistory[target]);
    _browserUpdateNavButtons();
}

function _browserNodeMatchesFilter(node, filter) {
    if (!filter) return true;
    var name = (node.display_name || '').toLowerCase();
    var hash = (node.identity_hash || '').toLowerCase();
    return name.indexOf(filter) !== -1 || hash.indexOf(filter) !== -1;
}

function _browserRenderNodes() {
    var list = document.getElementById('browser-nodes-list');
    if (!list) return;

    var filtered = _browserNodesRaw.filter(function(node) {
        return _browserNodeMatchesFilter(node, _browserNodesFilter);
    });

    if (filtered.length === 0) {
        list.innerHTML = '<div class="browser-nodes-empty text-muted-color">' +
            (_browserNodesRaw.length === 0 ? 'No nodes discovered yet.' : 'No nodes match your filter.') +
            '</div>';
        return;
    }

    // Most-recently-seen first.
    filtered.sort(function(a, b) {
        return (b.last_seen || 0) - (a.last_seen || 0);
    });

    var html = '';
    filtered.forEach(function(node) {
        var hash = node.identity_hash || '';
        var name = node.display_name || (hash ? shortHash(hash, 8, 4) : 'Unknown node');
        var hops = node.hops != null ? node.hops + ' hops' : '';
        var relTime = node.last_seen ? RS.relativeTime(node.last_seen) : '';
        html +=
            '<div class="browser-node-row" data-hash="' + escapeHtml(hash) + '">' +
                '<div class="browser-node-row-top">' +
                    '<span class="browser-node-name">' + escapeHtml(name) + '</span>' +
                    '<span class="browser-node-hops">' + escapeHtml(hops) + '</span>' +
                '</div>' +
                '<div class="browser-node-row-bottom">' +
                    copyableHash(hash, 16) +
                    '<span class="browser-node-time">' + escapeHtml(relTime) + '</span>' +
                '</div>' +
            '</div>';
    });
    list.innerHTML = html;
}

function browserTabLoad() {
    RS.invoke('api_nomad_nodes').then(function(nodes) {
        _browserNodesRaw = Array.isArray(nodes) ? nodes : [];
        _browserRenderNodes();
    }).catch(function() {
        var list = document.getElementById('browser-nodes-list');
        if (list && _browserNodesRaw.length === 0) {
            list.innerHTML = '<div class="browser-nodes-empty text-muted-color">Could not load nodes.</div>';
        }
    });
}

function _browserResetAutoRefresh() {
    if (_browserNodesAutoRefreshTimer) clearInterval(_browserNodesAutoRefreshTimer);
    _browserNodesAutoRefreshTimer = setInterval(function() {
        var view = document.getElementById('view-browser');
        if (!view || !view.classList.contains('active')) return;
        if (document.hidden) return;
        browserTabLoad();
    }, BROWSER_NODES_AUTO_REFRESH_MS);
}

document.addEventListener('DOMContentLoaded', function() {
    var form = document.getElementById('browser-addressbar');
    if (form) {
        form.addEventListener('submit', function(e) {
            e.preventDefault();
            var input = document.getElementById('browser-address-input');
            _browserNavigate(input ? input.value.trim() : '');
        });
    }

    // nomad:// pages can't be read cross-origin (iframe.contentWindow.location),
    // so in-page link clicks report their own location back via postMessage
    // instead — keeps the address bar in sync with where the iframe actually is.
    window.addEventListener('message', function(e) {
        if (!e.data) return;
        var frame = document.getElementById('browser-frame');
        if (e.source !== (frame && frame.contentWindow)) return;

        if (e.data.type === 'nomad-download') {
            _browserDownloadFile(e.data.href);
            return;
        }
        if (e.data.type !== 'nomad-nav') return;

        var input = document.getElementById('browser-address-input');
        if (input) input.value = e.data.href;
        // In-page link clicks navigate the frame directly, so this report is
        // the only signal they happened — record them too, or Back would skip
        // over every page reached by clicking a link.
        if (_browserSuppressHistoryPush) {
            _browserSuppressHistoryPush = false;
        } else {
            _browserHistoryPush(e.data.href);
        }
    });

    // A nomad:// fetch is bounded by the Rust side's request timeout, so the
    // frame fires load either way -- page or error page -- and the indicator
    // clears on both. Blanking the frame to abandon a stale load fires a load
    // of its own, which must not be mistaken for the new one arriving.
    var loadFrame = document.getElementById('browser-frame');
    if (loadFrame) {
        loadFrame.addEventListener('load', function() {
            if ((loadFrame.getAttribute('src') || '') === 'about:blank') return;
            _browserSetLoading(false);
        });
    }

    var backBtn = document.getElementById('browser-back');
    if (backBtn) backBtn.addEventListener('click', function() { _browserGo(-1); });
    var fwdBtn = document.getElementById('browser-forward');
    if (fwdBtn) fwdBtn.addEventListener('click', function() { _browserGo(1); });
    _browserUpdateNavButtons();

    var searchInput = document.getElementById('browser-nodes-search');
    if (searchInput) {
        searchInput.addEventListener('input', function() {
            clearTimeout(_browserNodesSearchTimer);
            var val = this.value.toLowerCase().trim();
            _browserNodesSearchTimer = setTimeout(function() {
                _browserNodesFilter = val;
                _browserRenderNodes();
            }, 150);
        });
        searchInput.addEventListener('keydown', function(e) {
            if (e.key === 'Enter') { e.preventDefault(); this.blur(); }
        });
    }

    var list = document.getElementById('browser-nodes-list');
    if (list) {
        list.addEventListener('click', function(e) {
            // Clicking the copy-hash control copies; it must not also navigate.
            if (e.target.closest('.hash-copy')) return;
            var row = e.target.closest('.browser-node-row');
            if (!row) return;
            var hash = row.dataset.hash;
            if (hash) _browserNavigate(_browserDefaultUrlFor(hash));
        });
    }

    _browserResetAutoRefresh();
});
