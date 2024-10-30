module Wid
  module AST
    class Expr
      class Binary < Expr
        # @rbs left: Expr
        # @rbs operator: Symbol
        # @rbs right: Expr
        # @rbs return: void
        def initialize(left:, operator:, right:)
          @left = left
          @operator = operator
          @right = right
        end

        def to_h
          super.merge(left: @left.to_h, operator: @operator, right: @right.to_h)
        end
      end
    end
  end
end
