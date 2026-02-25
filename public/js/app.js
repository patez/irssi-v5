const app = {
    user: null,

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

        // Hit /api/terminal first — this provisions the soju account and
        // starts ttyd. Only then point the iframe at /terminal/ to proxy it.
        try {
            const res = await fetch('/api/terminal');
            if (!res.ok) throw new Error(`${res.status}`);
        } catch (e) {
            this.updateStatus('disconnected', 'Failed to start terminal');
            console.error(e);
            return;
        }

        const frame = document.getElementById('terminal-frame');
        frame.src = '/terminal/';
        frame.onload = () => this.updateStatus('connected', 'Connected');
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