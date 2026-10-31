-- expect: ids=notnull agg=nullable:expression
SELECT ARRAY(SELECT i.id FROM invoices i WHERE i.customer_id = c.id) AS ids,
       (SELECT array_agg(i.id) FROM invoices i WHERE i.customer_id = c.id) AS agg
FROM customers c
