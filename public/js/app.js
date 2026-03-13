const DEBUG = location.hostname === 'localhost' || location.hostname.endsWith('.ts.net');
const log = (...args) => DEBUG && console.log(...args);

const app = {
    user: null,
    _term: null,
    _fitAddon: null,
    _ws: null,
    _reconnecting: false,
    _reconnectTimer: null,
    _lastHidden: 0,
    _expectingReconnect: false,

    async init() {
        try {
            const res = await fetch('/api/me');
            if (!res.ok) {
                document.body.innerHTML = `
                    <div style="color:#f00;padding:40px;font-family:monospace;background:#000;height:100vh">
                        Not authenticated. Access via Cloudflare Access.
                    </div>`;
                return;
            }
            this.user = await res.json();
        } catch {
            this.updateStatus('disconnected', 'Failed to reach server');
            return;
        }

        document.getElementById('user-info').textContent = `(${this.user.username})`;

        if (this.user.isAdmin) {
            const link = document.getElementById('admin-link');
            link.style.display = 'inline';
            link.addEventListener('click', () => AdminPanel.show());
        }

        document.getElementById('btn-reset').addEventListener('click', () => this.resetSession());

        // Paste button — only shown on touch devices
        const btnPaste = document.getElementById('btn-paste');
        if (btnPaste && ('ontouchstart' in window || navigator.maxTouchPoints > 0)) {
            btnPaste.style.display = '';
            btnPaste.addEventListener('click', () => this._pasteFromClipboard());
        }

        this._initTerm();
        this._setupReconnect();
        await this.loadTerminal();
    },

    async _pasteFromClipboard() {
        try {
            const text = await navigator.clipboard.readText();
            if (text && this._ws && this._ws.readyState === WebSocket.OPEN) {
                this._ws.send('0' + text);
            }
        } catch (e) {
            log('paste failed:', e);
        }
    },

    _initTerm() {
        this._term = new Terminal({
            cursorBlink: true,
            fontSize: 14,
            fontFamily: 'Menlo, Monaco, "Courier New", monospace',
            theme: {
                background: '#000000',
                foreground: '#ffffff',
            },
            scrollback: 5000,
            allowTransparency: false,
            scrollOnUserInput: true,
            smoothScrollDuration: 0,
        });

        this._fitAddon = new FitAddon.FitAddon();
        this._term.loadAddon(this._fitAddon);
        const webLinksAddon = new WebLinksAddon.WebLinksAddon();
        this._term.loadAddon(webLinksAddon);
        this._term.open(document.getElementById('terminal'));
        setTimeout(() => this._fitAddon.fit(), 100);

        // Send input to ttyd — protocol: '0' + data
        this._term.onData(data => {
            if (!this._ws || this._ws.readyState !== WebSocket.OPEN) return;
            this._ws.send('0' + data);
        });

        // iOS touch scroll — sends PgUp/PgDn to irssi.
        // Only preventDefault once scroll intent is clear (moved > SCROLL_THRESHOLD px)
        // so that taps, long-press paste menu, and link clicks are not blocked.
        const termEl = document.getElementById('terminal');
        const SCROLL_THRESHOLD = 8;
        let touchStartY = 0;
        let touchAccum = 0;
        let isScrolling = false;

        termEl.addEventListener('touchstart', (e) => {
            touchStartY = e.touches[0].clientY;
            touchAccum = 0;
            isScrolling = false;
        }, { passive: true });

        termEl.addEventListener('touchmove', (e) => {
            const dy = touchStartY - e.touches[0].clientY;
            touchAccum += dy;
            touchStartY = e.touches[0].clientY;

            if (!isScrolling && Math.abs(touchAccum) > SCROLL_THRESHOLD) {
                isScrolling = true;
            }

            if (!isScrolling) return;

            e.preventDefault();

            const lines = Math.trunc(touchAccum / 60);
            if (lines !== 0) {
                touchAccum -= lines * 60;
                const key = lines > 0 ? '\x1b[6~' : '\x1b[5~'; // PgDn / PgUp
                for (let i = 0; i < Math.abs(lines); i++) {
                    if (this._ws && this._ws.readyState === WebSocket.OPEN) {
                        this._ws.send('0' + key);
                    }
                }
            }
        }, { passive: false });

        // Resize on container size change
        const ro = new ResizeObserver(() => this._onResize());
        ro.observe(document.getElementById('terminal-container'));
        window.addEventListener('resize', () => this._onResize());

        // iOS keyboard resize
        if (window.visualViewport) {
            window.visualViewport.addEventListener('resize', () => {
                setTimeout(() => this._onResize(), 50);
            });
            window.visualViewport.addEventListener('scroll', () => {
                setTimeout(() => this._onResize(), 50);
            });
        }
    },

    _onResize() {
        if (!this._fitAddon) return;

        const container = document.getElementById('terminal-container');
        const statusBar = document.getElementById('status-bar');

        if (window.visualViewport) {
            const vv = window.visualViewport;
            const keyboardOpen = vv.height < window.innerHeight * 0.75;

            container.style.position = 'fixed';
            container.style.top = vv.offsetTop + 'px';
            container.style.left = vv.offsetLeft + 'px';
            container.style.width = vv.width + 'px';
            container.style.height = keyboardOpen ? vv.height + 'px' : (vv.height - 50) + 'px';

            statusBar.style.display = keyboardOpen ? 'none' : 'flex';
            if (!keyboardOpen) {
                statusBar.style.position = 'fixed';
                statusBar.style.top = (vv.offsetTop + vv.height - 50) + 'px';
                statusBar.style.left = vv.offsetLeft + 'px';
                statusBar.style.width = vv.width + 'px';
            }
        }

        this._fitAddon.fit();
        if (!this._ws || this._ws.readyState !== WebSocket.OPEN) return;
        this._ws.send('1' + JSON.stringify({ columns: this._term.cols, rows: this._term.rows }));
    },

    _setupReconnect() {
        document.addEventListener('visibilitychange', () => {
            if (document.hidden) {
                this._lastHidden = Date.now();
                this._expectingReconnect = true;
            } else {
                if (!this._ws || this._ws.readyState !== WebSocket.OPEN) {
                    this._scheduleReconnect(300);
                }
            }
        });

        window.addEventListener('pageshow', (e) => {
            if (e.persisted) this._scheduleReconnect(0);
        });

        window.addEventListener('focus', () => {
            if (!this._ws || this._ws.readyState !== WebSocket.OPEN) {
                this._scheduleReconnect(300);
            }
        });

        setInterval(() => {
            if (!document.hidden && (!this._ws || this._ws.readyState === WebSocket.CLOSED)) {
                this._scheduleReconnect(0);
            }
        }, 2000);
    },

    _scheduleReconnect(ms) {
        clearTimeout(this._reconnectTimer);
        this._reconnectTimer = setTimeout(() => this._connect(), ms);
    },

    async loadTerminal() {
        this.updateStatus('connecting', 'Starting terminal...');
        try {
            const res = await fetch('/api/terminal');
            if (!res.ok) throw new Error(`${res.status}`);
        } catch {
            this.updateStatus('disconnected', 'Failed to start terminal');
            return;
        }
        this._connect();
    },

    _connect() {
        if (this._ws) {
            this._ws.onclose = null;
            this._ws.onerror = null;
            this._ws.close();
            this._ws = null;
        }

        const proto = location.protocol === 'https:' ? 'wss' : 'ws';
        const ws = new WebSocket(`${proto}://${location.host}/terminal/ws`, ['tty']);
        ws.binaryType = 'arraybuffer';
        this._ws = ws;
        this._reconnecting = false;

        ws.onopen = () => {
            log('ws open, sending auth');
            this._expectingReconnect = false;
            ws.send(JSON.stringify({ AuthToken: '' }));
            this.updateStatus('connected', 'Connected');
            this._onResize();
        };

        ws.onmessage = (e) => {
            if (!(e.data instanceof ArrayBuffer)) return;
            const buf = new Uint8Array(e.data);
            if (buf.length === 0) return;
            const type = buf[0];
            const payload = buf.slice(1);

            log('type:', type, 'payload preview:', new TextDecoder().decode(payload.slice(0, 80)));

            if (type === 48) {
                this._term.write(payload);
            }
            // 49 = title, 50 = prefs — ignore
        };

        ws.onclose = () => {
            if (!this._reconnecting && !this._expectingReconnect) {
                this.updateStatus('disconnected', 'Disconnected');
            }
            log('ws closed, expectingReconnect:', this._expectingReconnect);
        };

        ws.onerror = () => {
            if (!this._expectingReconnect) {
                this.updateStatus('disconnected', 'Connection error');
            }
            log('ws error, expectingReconnect:', this._expectingReconnect);
        };
    },

    async resetSession() {
        if (!confirm('Reset your IRC session?')) return;
        try {
            await fetch('/api/session/clear', { method: 'POST' });
            this.updateStatus('connecting', 'Restarting...');
            if (this._ws) { this._ws.onclose = null; this._ws.close(); this._ws = null; }
            this._term.clear();
            setTimeout(() => this.loadTerminal(), 1500);
        } catch {
            this.updateStatus('disconnected', 'Reset failed');
        }
    },

    updateStatus(state, text) {
        const dot = document.getElementById('status-dot');
        const span = document.getElementById('status-text');
        if (dot) dot.className = 'status-dot ' + state;
        if (span) span.textContent = text;
    }
};

window.addEventListener('DOMContentLoaded', () => app.init());