/**
 * Process List Component
 * File: gaia/static/modules/task-manager/process-list.js
 * Purpose: Process listing and management
 */
class ProcessList {
  constructor(options) {
    this.container = options.container;
    this.processes = [
      { name: 'Firefox', pid: 1234, cpu: 15.2, memory: 512 },
      { name: 'VS Code', pid: 1235, cpu: 8.5, memory: 724 },
      { name: 'Chrome', pid: 1236, cpu: 22.1, memory: 1024 },
      { name: 'Terminal', pid: 1237, cpu: 2.1, memory: 45 },
      { name: 'File Manager', pid: 1238, cpu: 1.5, memory: 78 },
    ];
  }

  render() {
    const html = `
      <div class="process-table">
        <div class="process-header">
          <span class="process-col-name">Process Name</span>
          <span class="process-col-pid">PID</span>
          <span class="process-col-cpu">CPU %</span>
          <span class="process-col-mem">Memory (MB)</span>
        </div>
        ${this.processes.map(p => `
          <div class="process-row" data-pid="${p.pid}">
            <span class="process-col-name">${p.name}</span>
            <span class="process-col-pid">${p.pid}</span>
            <span class="process-col-cpu">${p.cpu.toFixed(1)}%</span>
            <span class="process-col-mem">${p.memory}</span>
          </div>
        `).join('')}
      </div>
    `;

    this.container.innerHTML = html;
    this.attachListeners();
  }

  attachListeners() {
    const rows = this.container.querySelectorAll('.process-row');
    rows.forEach(row => {
      row.addEventListener('contextmenu', (e) => this.showContextMenu(e));
    });
  }

  showContextMenu(e) {
    e.preventDefault();
    const pid = e.target.closest('.process-row').dataset.pid;
    console.log(`Context menu for PID: ${pid}`);
  }
}

export default ProcessList;