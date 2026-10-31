-- expect: email=nullable:outer-join total=nullable:outer-join
WITH x AS (
  SELECT c.email, i.total FROM customers c FULL JOIN invoices i ON i.customer_id = c.id
)
SELECT x.email, x.total FROM x
