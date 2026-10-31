-- expect: email=nullable:outer-join total=nullable:outer-join
SELECT c.email, i.total FROM customers c FULL JOIN invoices i ON i.customer_id = c.id
