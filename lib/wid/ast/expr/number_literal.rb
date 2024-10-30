module Wid
  module AST
    class Expr
      class NumberLiteral < Literal
        def initialize(value:)
          value = value.include?(".") ? Float(value) : Integer(value)
          super
        end
      end
    end
  end
end
