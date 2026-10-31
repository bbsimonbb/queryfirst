-- expect: body=nullable id=required:where-eq-notnull
UPDATE drafts SET body = :body WHERE id = :id
