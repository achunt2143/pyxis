/**
 * System Monitor Component
 * File: gaia/static/modules/task-manager/system-monitor.js
 * Purpose: System resource monitoring
 */
class SystemMonitor {
  constructor(options) {
    this.container = options.container;
    this.updateInterval = options.updateInterval || 1000;
  }

  start() {
    this.render();
    setInterval(() => this.update(), this.updateInterval);
  }

  render() {
    this.container.innerHTML = `
      <div class="monitor-grid">
        <div class="monitor-item">
          <h4>CPU Usage</h4>
          <div class="monitor-bar">
            <div class="monitor-fill cpu-fill" style="width: 45%"></div>
          </div>
          <p class="monitor-value">45%</p>
        </div>
        <div class="monitor-item">
          <h4>Memory Usage</h4>
          <div class="monitor-bar">
            <div class="monitor-fill memory-fill" style="width: 62%"></div>
          </div>
          <p class="monitor-value">4.9 GB / 8 GB</p>
        </div>
        <div class="monitor-item">
          <h4>Disk Usage</h4>
          <div class="monitor-bar">
            <div class="monitor-fill disk-fill" style="width: 72%"></div>
          </div>
          <p class="monitor-value">720 GB / 1 TB</p>
        </div>
      </div>
    `;
  }

  update() {
    const cpuUsage = Math.random() * 100;
    const memUsage = Math.random() * 100;
    const diskUsage = Math.random() * 100;

    const cpuFill = this.container.querySelector('.cpu-fill');
    const memFill = this.container.querySelector('.memory-fill');
    const diskFill = this.container.querySelector('.disk-fill');

    if (cpuFill) cpuFill.style.width = `${cpuUsage}%`;
    if (memFill) memFill.style.width = `${memUsage}%`;
    if (diskFill) diskFill.style.width = `${diskUsage}%`;
  }
}

export default SystemMonitor;