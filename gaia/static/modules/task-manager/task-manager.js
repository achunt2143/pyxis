/**
 * Task Manager Component
 * File: gaia/static/modules/task-manager/task-manager.js
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class TaskManager {
  constructor(options) {
    this.element = options.element;
    this.processes = [];
  }

  async initialize() {
    await ModuleLoader.loadStyles('task-manager');
    this.createStructure();
    this.startMonitoring();
  }

  createStructure() {
    this.element.innerHTML = `
      <div class="task-manager-container">
        <div class="process-list" id="process-list"></div>
        <div class="system-monitor" id="system-monitor"></div>
      </div>
    `;
  }

  startMonitoring() {
    setInterval(() => this.updateProcesses(), 1000);
  }

  updateProcesses() {
    // Simulated process data
    this.processes = [
      { name: 'Firefox', pid: 1234, cpu: 15.2, memory: 512 },
      { name: 'VS Code', pid: 1235, cpu: 8.5, memory: 724 },
      { name: 'Chrome', pid: 1236, cpu: 22.1, memory: 1024 }
    ];

    const listEl = document.getElementById('process-list');
    listEl.innerHTML = this.processes.map(p => `
      <div class="process-row">
        <span>${p.name}</span>
        <span>${p.cpu}%</span>
        <span>${p.memory}MB</span>
      </div>
    `).join('');
  }

  async destroy() {}
}
export default TaskManager;