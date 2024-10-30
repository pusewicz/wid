module Wid
  module AST
    class Expr
      class Grouping < Expr
        def initialize(expr:)
          @expr = expr
        end

        def to_h
          super.merge(expr: @expr.to_h)
        end
      end
    end
  end
end
