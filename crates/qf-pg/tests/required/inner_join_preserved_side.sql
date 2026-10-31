-- expect: line1=required:where-eq-notnull total=nullable
SELECT c.id FROM (customers c JOIN addresses a ON a.customer_id = c.id AND a.line1 = :line1)
LEFT JOIN invoices i ON i.customer_id = c.id AND i.total = :total
