/**
 * Gaia Email Service
 * File: gaia/static/services/email-service.js
 * Purpose: Email operations and integration
 */

class EmailService {
  constructor(echoService) {
    this.echo = echoService;
  }

  /**
   * Get inbox
   */
  async getInbox() {
    return this.echo.call('org.geckoOS.Email', 'getInbox', {});
  }

  /**
   * Get email
   */
  async getEmail(id) {
    return this.echo.call('org.geckoOS.Email', 'getMessage', { id });
  }

  /**
   * Send email
   */
  async sendEmail(to, subject, body) {
    return this.echo.call('org.geckoOS.Email', 'sendMessage', {
      to, subject, body
    });
  }

  /**
   * Delete email
   */
  async deleteEmail(id) {
    return this.echo.call('org.geckoOS.Email', 'deleteMessage', { id });
  }

  /**
   * Get contacts
   */
  async getContacts() {
    return this.echo.call('org.geckoOS.Email', 'getContacts', {});
  }
}

export default EmailService;