// admin.js

const AdminPanel = {
    async show() {
        const panel = document.getElementById('admin-panel');
        panel.innerHTML = this._skeleton();
        panel.classList.add('show');
        document.getElementById('btn-admin-close').onclick = () => panel.classList.remove('show');
        await this._load();
    },

    _skeleton() {
        return `
        <div class="admin-container">
            <div class="admin-header">
                <h2>Admin</h2>
                <button class="btn btn-danger" id="btn-admin-close">Close</button>
            </div>

            <div class="admin-section">
                <h3>Stats</h3>
                <div class="stats-grid">
                    <div class="stat-card"><div class="stat-value" id="s-total">—</div><div class="stat-label">Known users</div></div>
                    <div class="stat-card"><div class="stat-value" id="s-active">—</div><div class="stat-label">Active sessions</div></div>
                    <div class="stat-card"><div class="stat-value" id="s-max">—</div><div class="stat-label">Max users</div></div>
                </div>
            </div>

            <div class="admin-section">
                <h3>Settings</h3>
                <div class="settings-row">
                    <label>Max users</label>
                    <input type="number" id="inp-max-users" min="1" max="1000">
                    <button class="btn btn-primary" id="btn-save-settings">Save</button>
                </div>
            </div>

            <div class="admin-section">
                <h3>Users</h3>
                <div class="table">
                    <table>
                        <thead>
                            <tr>
                                <th>Username</th>
                                <th>First seen</th>
                                <th>Last seen</th>
                                <th>Session</th>
                                <th>Admin</th>
                                <th>Actions</th>
                            </tr>
                        </thead>
                        <tbody id="users-tbody">
                            <tr><td colspan="6" style="text-align:center;color:var(--text-tertiary)">Loading...</td></tr>
                        </tbody>
                    </table>
                </div>
            </div>
        </div>`;
    },

    async _load() {
        try {
            const [settings, usersData] = await Promise.all([
                fetch('/api/admin/settings').then(r => r.json()),
                fetch('/api/admin/users').then(r => r.json())
            ]);

            document.getElementById('s-total').textContent  = settings.totalUsers;
            document.getElementById('s-active').textContent = settings.activeSessions;
            document.getElementById('s-max').textContent    = settings.maxUsers;
            document.getElementById('inp-max-users').value  = settings.maxUsers;

            document.getElementById('btn-save-settings').onclick = async () => {
                const max = parseInt(document.getElementById('inp-max-users').value);
                await fetch('/api/admin/settings', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ maxUsers: max })
                });
                await this._load();
            };

            this._renderUsers(usersData.users);
        } catch (e) {
            console.error('Admin load failed', e);
        }
    },

    _renderUsers(users) {
        const tbody = document.getElementById('users-tbody');
        if (!users || !users.length) {
            tbody.innerHTML = '<tr><td colspan="6" style="text-align:center;color:var(--text-tertiary)">No users yet</td></tr>';
            return;
        }

        tbody.innerHTML = users.map(u => `
            <tr>
                <td>${u.username}</td>
                <td>${new Date(u.first_seen).toLocaleDateString()}</td>
                <td>${new Date(u.last_seen).toLocaleDateString()}</td>
                <td>${u.active_session ? '<span style="color:var(--success)">● Active</span>' : '—'}</td>
                <td>${u.is_admin ? '✓' : ''}</td>
                <td>
                    <div class="actions-cell">
                        ${u.active_session ? `<button class="btn btn-kick" data-u="${u.username}">Kick</button>` : ''}
                        <button class="btn btn-clear" data-u="${u.username}">Clear</button>
                        <button class="btn btn-danger btn-del" data-u="${u.username}">Delete</button>
                    </div>
                </td>
            </tr>
        `).join('');

        tbody.querySelectorAll('.btn-kick').forEach(btn => {
            btn.onclick = async () => {
                await fetch(`/api/admin/users/${btn.dataset.u}/kick`, { method: 'POST' });
                await this._load();
            };
        });

        tbody.querySelectorAll('.btn-clear').forEach(btn => {
            btn.onclick = async () => {
                if (!confirm(`Clear IRC session and config for "${btn.dataset.u}"?`)) return;
                await fetch(`/api/admin/users/${btn.dataset.u}/clear`, { method: 'POST' });
                await this._load();
            };
        });

        tbody.querySelectorAll('.btn-del').forEach(btn => {
            btn.onclick = async () => {
                if (!confirm(`Remove "${btn.dataset.u}" from user list?`)) return;
                await fetch(`/api/admin/users/${btn.dataset.u}`, { method: 'DELETE' });
                await this._load();
            };
        });
    }
};
