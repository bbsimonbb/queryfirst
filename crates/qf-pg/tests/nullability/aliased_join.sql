-- expect: email=nullable:analyzer-gave-up
SELECT j.email FROM (customers c JOIN addresses a ON a.customer_id = c.id) AS j
