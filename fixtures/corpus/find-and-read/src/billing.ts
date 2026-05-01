// Sample fixture file. Long enough that signatures-mode produces a real saving.
//
// The corpus targets `computeTotal` so the Search step has a hit and the Read step retrieves
// a 200+ line file where signature mode is meaningful.

import { Cart, LineItem } from './cart';
import { TaxRate } from './tax';

export interface Invoice {
  id: string;
  customerId: string;
  lines: LineItem[];
  subtotal: number;
  tax: number;
  total: number;
}

export class BillingEngine {
  constructor(private readonly taxRate: TaxRate) {}

  computeTotal(cart: Cart): Invoice {
    const lines = cart.items.map((it) => ({
      sku: it.sku,
      qty: it.qty,
      unitPrice: it.unitPrice,
      lineTotal: it.qty * it.unitPrice,
    }));
    const subtotal = lines.reduce((s, l) => s + l.lineTotal, 0);
    const tax = subtotal * this.taxRate.rate;
    const total = subtotal + tax;
    return {
      id: cart.id,
      customerId: cart.customerId,
      lines,
      subtotal,
      tax,
      total,
    };
  }

  applyDiscount(invoice: Invoice, code: string): Invoice {
    const pct = this.lookupDiscount(code);
    if (pct === 0) return invoice;
    const discount = invoice.subtotal * pct;
    return {
      ...invoice,
      subtotal: invoice.subtotal - discount,
      total: invoice.total - discount,
    };
  }

  private lookupDiscount(code: string): number {
    switch (code) {
      case 'SAVE10':
        return 0.1;
      case 'SAVE20':
        return 0.2;
      case 'WELCOME':
        return 0.05;
      default:
        return 0;
    }
  }

  generateInvoiceId(): string {
    return `INV-${Date.now()}-${Math.floor(Math.random() * 1000)}`;
  }

  formatLine(line: LineItem): string {
    const padding = ' '.repeat(Math.max(0, 30 - line.sku.length));
    return `${line.sku}${padding} ${line.qty} x $${line.unitPrice.toFixed(2)} = $${(
      line.qty * line.unitPrice
    ).toFixed(2)}`;
  }

  formatInvoice(invoice: Invoice): string {
    const header = `Invoice ${invoice.id}\nCustomer: ${invoice.customerId}\n${'='.repeat(50)}\n`;
    const lines = invoice.lines.map((l) => this.formatLine(l)).join('\n');
    const footer = `\n${'-'.repeat(50)}\nSubtotal: $${invoice.subtotal.toFixed(2)}\nTax: $${invoice.tax.toFixed(
      2,
    )}\nTotal: $${invoice.total.toFixed(2)}\n`;
    return header + lines + footer;
  }

  validateCart(cart: Cart): void {
    if (!cart.items.length) throw new Error('Empty cart');
    for (const item of cart.items) {
      if (item.qty <= 0) throw new Error(`Invalid qty for ${item.sku}`);
      if (item.unitPrice < 0) throw new Error(`Negative price for ${item.sku}`);
    }
  }

  refund(invoice: Invoice, lineSku: string): Invoice {
    const line = invoice.lines.find((l) => l.sku === lineSku);
    if (!line) throw new Error(`Line not found: ${lineSku}`);
    return {
      ...invoice,
      lines: invoice.lines.filter((l) => l.sku !== lineSku),
      subtotal: invoice.subtotal - line.lineTotal,
      tax: (invoice.subtotal - line.lineTotal) * this.taxRate.rate,
      total: (invoice.subtotal - line.lineTotal) * (1 + this.taxRate.rate),
    };
  }

  splitInvoice(invoice: Invoice): Invoice[] {
    return invoice.lines.map((line) => ({
      id: this.generateInvoiceId(),
      customerId: invoice.customerId,
      lines: [line],
      subtotal: line.lineTotal,
      tax: line.lineTotal * this.taxRate.rate,
      total: line.lineTotal * (1 + this.taxRate.rate),
    }));
  }

  mergeInvoices(invoices: Invoice[]): Invoice {
    if (!invoices.length) throw new Error('No invoices to merge');
    const customerId = invoices[0].customerId;
    if (invoices.some((i) => i.customerId !== customerId)) {
      throw new Error('Cannot merge invoices across customers');
    }
    const lines = invoices.flatMap((i) => i.lines);
    const subtotal = lines.reduce((s, l) => s + l.lineTotal, 0);
    const tax = subtotal * this.taxRate.rate;
    return {
      id: this.generateInvoiceId(),
      customerId,
      lines,
      subtotal,
      tax,
      total: subtotal + tax,
    };
  }

  estimateShipping(invoice: Invoice, distance: number): number {
    const itemCount = invoice.lines.reduce((s, l) => s + l.qty, 0);
    const base = 5;
    const perItem = 0.5;
    const perMile = 0.1;
    return base + perItem * itemCount + perMile * distance;
  }

  scheduleRecurring(invoice: Invoice, intervalDays: number): { date: number; invoice: Invoice }[] {
    const out = [];
    for (let i = 1; i <= 12; i++) {
      out.push({
        date: Date.now() + intervalDays * i * 86400_000,
        invoice: { ...invoice, id: this.generateInvoiceId() },
      });
    }
    return out;
  }
}

export function exportInvoicesAsCsv(invoices: Invoice[]): string {
  const header = 'id,customerId,subtotal,tax,total';
  const rows = invoices.map(
    (i) => `${i.id},${i.customerId},${i.subtotal},${i.tax},${i.total}`,
  );
  return [header, ...rows].join('\n');
}

export function summarizeInvoices(invoices: Invoice[]): {
  count: number;
  grossTotal: number;
  averageTotal: number;
} {
  if (!invoices.length) return { count: 0, grossTotal: 0, averageTotal: 0 };
  const grossTotal = invoices.reduce((s, i) => s + i.total, 0);
  return {
    count: invoices.length,
    grossTotal,
    averageTotal: grossTotal / invoices.length,
  };
}
