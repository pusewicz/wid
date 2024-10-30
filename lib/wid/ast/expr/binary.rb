module Wid
  module AST
    class Expr
      class Binary < Expr
        def initialize(left:, operator:, right:)
          @left = left
          @operator = operator
          @right = right
        end

        def to_h
          super.merge(left: @left.to_h, operator: @operator.to_h, right: @right.to_h)
        end
      end
    end
  end
end
