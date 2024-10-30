module Wid
  module AST
    class Expr
      class StringLiteral < Literal
        def initialize(value:)
          value = value[1...-1]
          super
        end
      end
    end
  end
end
