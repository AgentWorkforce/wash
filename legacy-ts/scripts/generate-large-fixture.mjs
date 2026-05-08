// Generates a synthetic but realistic 2500-line TypeScript module so the burn-compare
// fixture corpus exercises Read's signature-mode elision honestly.
//
// Run: node scripts/generate-large-fixture.mjs <outPath>

import { writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const out = resolve(process.argv[2] || 'fixtures/corpus/large-codebase/src/billing-engine.ts');
const lines = [];

// Imports
lines.push(`// Auto-generated synthetic billing engine. Used by the relaywash burn-compare fixture corpus.`);
lines.push(`import { Cart, LineItem } from './cart';`);
lines.push(`import { TaxRate, lookupRate } from './tax';`);
lines.push(`import { Customer } from './customer';`);
lines.push(`import { Logger } from './log';`);
lines.push('');

// Type declarations
lines.push(`export interface Invoice {`);
lines.push(`  id: string;`);
lines.push(`  customerId: string;`);
lines.push(`  lines: LineItem[];`);
lines.push(`  subtotal: number;`);
lines.push(`  tax: number;`);
lines.push(`  total: number;`);
lines.push(`  createdAt: number;`);
lines.push(`  status: 'draft' | 'sent' | 'paid' | 'void';`);
lines.push(`}`);
lines.push('');

lines.push(`export interface DiscountRule {`);
lines.push(`  code: string;`);
lines.push(`  percent: number;`);
lines.push(`  minSubtotal?: number;`);
lines.push(`  expiresAt?: number;`);
lines.push(`}`);
lines.push('');

// Big class with mix of small and large methods
lines.push(`export class BillingEngine {`);
lines.push(`  private readonly logger = new Logger('BillingEngine');`);
lines.push(`  constructor(private readonly taxRate: TaxRate) {}`);
lines.push('');

// 5 large methods (40+ lines each — body should be elided in signatures mode)
for (let i = 0; i < 5; i++) {
  const name = ['computeTotal', 'applyDiscount', 'splitInvoice', 'mergeInvoices', 'reconcileLines'][i];
  lines.push(`  ${name}(input: Invoice, opts?: { strict?: boolean }): Invoice {`);
  lines.push(`    this.logger.debug('${name} called', { id: input.id });`);
  for (let j = 0; j < 35; j++) {
    lines.push(`    const step${j} = input.lines.reduce((acc, l) => acc + l.qty * l.unitPrice * ${1 + j * 0.001}, 0);`);
  }
  lines.push(`    const subtotal = input.lines.reduce((s, l) => s + l.qty * l.unitPrice, 0);`);
  lines.push(`    const tax = subtotal * this.taxRate.rate;`);
  lines.push(`    return { ...input, subtotal, tax, total: subtotal + tax };`);
  lines.push(`  }`);
  lines.push('');
}

// 30 small methods (< 20 lines — bodies should be KEPT under the heuristic)
for (let i = 0; i < 30; i++) {
  lines.push(`  helper${i}(arg: number): number {`);
  lines.push(`    return arg * ${i + 1} + this.taxRate.rate;`);
  lines.push(`  }`);
  lines.push('');
}

// 10 more large methods to push the file over 2000 lines
for (let i = 0; i < 10; i++) {
  lines.push(`  process${i}(invoice: Invoice, customer: Customer): Invoice {`);
  for (let j = 0; j < 80; j++) {
    lines.push(`    // step ${j}: validate, transform, persist intermediate state`);
    lines.push(`    if (invoice.lines.length > ${j}) {`);
    lines.push(`      this.logger.trace('process${i}.step${j}');`);
    lines.push(`    }`);
  }
  lines.push(`    return invoice;`);
  lines.push(`  }`);
  lines.push('');
}

lines.push(`}`);
lines.push('');

// Top-level exported helpers — these become individual lineMap entries
const helpers = [
  'exportInvoicesAsCsv',
  'parseInvoiceFromJson',
  'validateDiscountCode',
  'computeShippingCost',
  'normalizeCustomerName',
  'formatCurrency',
  'roundToCents',
  'serializeForApi',
];
for (const h of helpers) {
  lines.push(`export function ${h}(input: any): string {`);
  for (let j = 0; j < 35; j++) {
    lines.push(`  // ${h} step ${j}: handle edge cases and format output`);
  }
  lines.push(`  return JSON.stringify(input);`);
  lines.push(`}`);
  lines.push('');
}

writeFileSync(out, lines.join('\n'));
console.log(`Wrote ${lines.length} lines → ${out}`);
