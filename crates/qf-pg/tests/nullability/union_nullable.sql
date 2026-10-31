-- expect: id=notnull name=nullable:analyzer-gave-up
SELECT id, name FROM customers
UNION ALL
SELECT id, city FROM addresses
UNION ALL
SELECT id, notes FROM invoices
