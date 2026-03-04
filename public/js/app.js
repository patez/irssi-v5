const app = {
    user: null,

    _patchTtyd(frame) {
        const doc = frame.contentDocument;
        if (!doc) return;

        let vp = doc.querySelector('meta[name=viewport]');
        if (!vp) {
            vp = doc.createElement('meta');
            vp.name = 'viewport';
            doc.head.appendChild(vp);
        }
        vp.content = 'width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no';

        const link = doc.createElement('link');
        link.rel = 'stylesheet';
        link.href = '/css/ttyd-overrides.css';
        doc.head.appendChild(link);

        // Give xterm a moment to pick up the new dimensions
        setTimeout(() => {
            frame.contentWindow?.dispatchEvent(new Event('resize'));
        }, 100);
    },

    async _reconnect() {
        if (this._reconnecting) return;
        this._reconnecting = true;
        this.updateStatus('connecting', 'Reconnecting...');
        try {
            const res = await fetch('/api/terminal');
            if (!res.ok) throw new Error(`${res.status}`);
        } catch {
            this.updateStatus('disconnected', 'Reconnect failed');
            this._reconnecting = false;
            return;
        }
        const frame = document.getElementById('terminal-frame');
        frame.src = '';
        await new Promise(r => setTimeout(r, 100));
        frame.onload = () => {
            this._connected = true;
            this.updateStatus('connected', 'Connected');
            this._patchTtyd(frame);
            this._reconnecting = false;
        };
        frame.src = '/terminal/';
    },

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

        await this.loadTerminal();
    },

    async loadTerminal() {
        this.updateStatus('connecting', 'Starting terminal...');
        this._connected = false;
        try {
            const res = await fetch('/api/terminal');
            if (!res.ok) throw new Error(`${res.status}`);
        } catch (e) {
            this.updateStatus('disconnected', 'Failed to start terminal');
            return;
        }

        const frame = document.getElementById('terminal-frame');
        frame.onload = () => {
            this._connected = true;
            this.updateStatus('connected', 'Connected');
            this._patchTtyd(frame);
        };
        frame.src = '/terminal/';
    },

    async resetSession() {
        if (!confirm('Reset your IRC session?')) return;
        try {
            await fetch('/api/session/clear', { method: 'POST' });
            this.updateStatus('connecting', 'Restarting...');
            const frame = document.getElementById('terminal-frame');
            frame.src = '';
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