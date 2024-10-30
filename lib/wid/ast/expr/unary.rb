module Wid
  module AST
    class Expr
      class Unary < Expr
        def initialize(operator:, right:)
          @operator = operator
          @right = right
        end

        def to_h
          super.merge(operator: @operator, right: @right.to_h)
        end
      end
    end
  end
end
