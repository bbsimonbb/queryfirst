-- expect: line1=nullable
SELECT j.email FROM (customers c JOIN addresses a ON a.customer_id = c.id AND a.line1 = :line1) AS j
