-- expect: slug=required:insert-notnull id=required:where-eq-notnull
UPDATE posts SET slug = :slug WHERE id = :id
