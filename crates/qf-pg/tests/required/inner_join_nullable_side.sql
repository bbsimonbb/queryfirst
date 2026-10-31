-- expect: line1=nullable
SELECT c.id FROM customers c LEFT JOIN (addresses a JOIN invoices i ON i.address_id = a.id AND a.line1 = :line1) ON a.customer_id = c.id
